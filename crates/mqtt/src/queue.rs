//! The queue between the callers (one per car, any number) and the one connection. Callers never wait: putting a
//! state or an event is a few map operations under a short lock. While the broker is away or busy, a car's states
//! coalesce: only its newest is kept, so thousands of cars updating every few seconds hold at most one state each,
//! however long the broker is gone. Events are moments and are all kept, up to a bound; past it the oldest go (and
//! are counted). Cars are served in the order they started waiting, so a busy car can't starve the others.

use crate::render::{CarInfo, Event};
use model::{CarId, Now};
use std::collections::{HashMap, VecDeque};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NowItem {
    pub info: CarInfo,
    pub now: Now,
    pub place: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Pending {
    Now(Box<NowItem>),
    /// Clear the car's topics and Home Assistant device.
    Forget,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Item {
    State(CarId, Pending),
    Event(Event),
}

#[derive(Debug)]
pub(crate) struct Queue {
    states: HashMap<CarId, Pending>,
    /// The cars in `states`, oldest waiting first.
    order: VecDeque<CarId>,
    events: VecDeque<Event>,
    max_cars: usize,
    max_events: usize,
    pub states_dropped: u64,
    pub events_dropped: u64,
}

impl Queue {
    pub fn new(max_cars: usize, max_events: usize) -> Queue {
        Queue {
            states: HashMap::new(),
            order: VecDeque::new(),
            events: VecDeque::new(),
            max_cars: max_cars.max(1),
            max_events: max_events.max(1),
            states_dropped: 0,
            events_dropped: 0,
        }
    }

    /// The car's newest state replaces any it has waiting (keeping its place in line).
    pub fn put_state(&mut self, car: CarId, pending: Pending) {
        if let Some(slot) = self.states.get_mut(&car) {
            *slot = pending;
            return;
        }
        if self.states.len() >= self.max_cars {
            if let Some(oldest) = self.order.pop_front() {
                self.states.remove(&oldest);
                self.states_dropped += 1;
            }
        }
        self.states.insert(car, pending);
        self.order.push_back(car);
    }

    pub fn put_event(&mut self, event: Event) {
        if self.events.len() >= self.max_events {
            self.events.pop_front();
            self.events_dropped += 1;
        }
        self.events.push_back(event);
    }

    /// The next thing to send: events first (they are few, and a moment late is worse than a value late).
    pub fn take(&mut self) -> Option<Item> {
        let item = self.next();
        self.shrink_when_idle();
        item
    }

    fn next(&mut self) -> Option<Item> {
        if let Some(e) = self.events.pop_front() {
            return Some(Item::Event(e));
        }
        while let Some(car) = self.order.pop_front() {
            if let Some(p) = self.states.remove(&car) {
                return Some(Item::State(car, p));
            }
        }
        None
    }

    /// After a long outage with many cars waiting, gives the memory back once the queue has drained.
    fn shrink_when_idle(&mut self) {
        const KEEP: usize = 1024;
        if self.states.is_empty() && self.states.capacity() > KEEP {
            self.states = HashMap::new();
            self.order = VecDeque::new();
        }
        if self.events.is_empty() && self.events.capacity() > KEEP {
            self.events = VecDeque::new();
        }
    }

    /// Items handed to a connection that dropped before the broker confirmed them go back to the front, in their
    /// order, unless a newer state of that car is already waiting (the newer one wins).
    pub fn requeue(&mut self, items: Vec<Item>) {
        for item in items.into_iter().rev() {
            match item {
                Item::Event(e) if self.events.len() < self.max_events => self.events.push_front(e),
                Item::Event(_) => self.events_dropped += 1,
                Item::State(car, _) if self.states.contains_key(&car) => {}
                Item::State(_, _) if self.states.len() >= self.max_cars => self.states_dropped += 1,
                Item::State(car, p) => {
                    self.states.insert(car, p);
                    self.order.push_front(car);
                }
            }
        }
    }

    pub fn states_len(&self) -> usize {
        self.states.len()
    }

    pub fn events_len(&self) -> usize {
        self.events.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::tests::drive;
    use std::time::Instant;

    fn now(car: CarId, soc: f64) -> Pending {
        let mut n = Now { car, ..Now::default() };
        n.sample.soc = Some(soc);
        Pending::Now(Box::new(NowItem { info: CarInfo::default(), now: n, place: None }))
    }

    fn soc(p: &Pending) -> Option<f64> {
        match p {
            Pending::Now(i) => i.now.sample.soc,
            Pending::Forget => None,
        }
    }

    fn event(car: CarId, id: u64) -> Event {
        let mut d = drive(car);
        d.id = id;
        Event::DriveStarted { drive: Box::new(d), start_place: None }
    }

    #[test]
    fn coalesces_2000_cars() {
        let mut q = Queue::new(100_000, 10_000);
        let t = Instant::now();
        for round in 0..10 {
            for car in 0..2_000 {
                q.put_state(car, now(car, f64::from(round)));
            }
        }
        let took = t.elapsed();
        assert_eq!(q.states_len(), 2_000, "one state per car, however many updates");
        assert_eq!(q.states_dropped, 0);
        let mut seen = 0;
        while let Some(item) = q.take() {
            let Item::State(car, p) = item else { panic!("no events were put") };
            assert_eq!(car, seen, "served in the order cars started waiting");
            assert_eq!(soc(&p), Some(9.0), "only the newest state is kept");
            seen += 1;
        }
        assert_eq!(seen, 2_000);
        assert_eq!(q.states.capacity(), 0, "drained: the memory is given back");
        eprintln!("20,000 updates from 2,000 cars queued in {took:?}");
        assert!(took.as_millis() < 2_000, "{took:?}");
    }

    #[test]
    fn events_bounded_oldest_dropped() {
        let mut q = Queue::new(10, 1_000);
        for i in 0..5_000u64 {
            q.put_event(event((i % 2_000) as CarId, i));
        }
        assert_eq!((q.events_len(), q.events_dropped), (1_000, 4_000));
        let Some(Item::Event(Event::DriveStarted { drive, .. })) = q.take() else { panic!() };
        assert_eq!(drive.id, 4_000, "the newest are kept");
        while q.take().is_some() {}
        assert_eq!(q.events_len(), 0);
    }

    #[test]
    fn events_before_states() {
        let mut q = Queue::new(10, 10);
        q.put_state(1, now(1, 50.0));
        q.put_event(event(2, 1));
        assert!(matches!(q.take(), Some(Item::Event(_))));
        assert!(matches!(q.take(), Some(Item::State(1, _))));
        assert_eq!(q.take(), None);
    }

    #[test]
    fn too_many_cars_drops_the_longest_waiting() {
        let mut q = Queue::new(3, 10);
        for car in 0..5 {
            q.put_state(car, now(car, 1.0));
        }
        assert_eq!((q.states_len(), q.states_dropped), (3, 2));
        assert!(matches!(q.take(), Some(Item::State(2, _))));
        // a waiting car's update doesn't count against the bound
        q.put_state(4, now(4, 2.0));
        assert_eq!(q.states_dropped, 2);
    }

    #[test]
    fn requeue_keeps_order_and_newer_states() {
        let mut q = Queue::new(10, 10);
        q.put_state(1, now(1, 10.0));
        q.put_state(2, now(2, 20.0));
        q.put_event(event(1, 7));
        let sent = vec![q.take().unwrap(), q.take().unwrap(), q.take().unwrap()];
        q.put_state(2, now(2, 21.0)); // newer than the one in flight
        q.put_state(3, now(3, 30.0));
        q.requeue(sent);
        assert!(matches!(q.take(), Some(Item::Event(Event::DriveStarted { .. }))));
        let Some(Item::State(1, p)) = q.take() else { panic!() };
        assert_eq!(soc(&p), Some(10.0), "car 1 back at the front");
        let Some(Item::State(2, p)) = q.take() else { panic!() };
        assert_eq!(soc(&p), Some(21.0), "the newer state of car 2 wins over the one in flight");
        assert!(matches!(q.take(), Some(Item::State(3, _))));
        assert_eq!(q.take(), None);
    }

    #[test]
    fn forget_replaces_a_waiting_state() {
        let mut q = Queue::new(10, 10);
        q.put_state(1, now(1, 10.0));
        q.put_state(1, Pending::Forget);
        assert_eq!(q.take(), Some(Item::State(1, Pending::Forget)));
    }
}
