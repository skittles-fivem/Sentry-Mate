//! Places, addresses and electricity prices, shared by every car.
//!
//! - [`geofence`]: which saved place (geofence) a position is at, through a grid index on the sphere so a lookup
//!   costs the same with ten places or a hundred thousand.
//! - [`suggest`](mod@suggest): spots where cars park most that are not saved yet, built from a stream of stops so any number of
//!   cars (or years of one car) never has to sit in memory as a list of positions.
//! - [`price`]: what a charge cost from its curve and the place's price plan (one price or time-of-use rates in
//!   the car's own time zone, daylight saving included, plus a session fee).
//! - [`geocode`]: addresses, offline (GeoNames towns, nearest lookup in a KD-tree) and online (Nominatim, one
//!   request a second for the whole server however many cars ask), as the owner chooses.
//! - [`tiles`]: the map tile settings the web UI reads (a PMTiles file, an online tile server, or both).
//! - [`pmtiles`]: reading single tiles out of an offline PMTiles file in the data folder.
//!
//! Places are shared: nothing here is per car, and nothing grows with the number of cars except what the caller
//! feeds in. Every type that answers lookups is immutable after it is built or guards its state itself, so cars
//! are worked on in parallel without a lock around this crate.
//!
//! Units follow `model`: metres for radii and distances here (as `model::Place::radius_m`), kWh, kW, ms UTC.

pub mod geo;
pub mod geocode;
pub mod geofence;
mod grid;
mod http;
pub mod pmtiles;
pub mod price;
pub mod suggest;
pub mod tiles;

pub use geo::{LatLon, distance_m};
pub use geocode::{Address, AddressSource, GeocodeSettings, Geocoder, Mode};
pub use geofence::{PlaceIndex, clamp_radius};
pub use http::HttpOptions;
pub use price::{ChargeCost, CostOptions, PriceError, Tariff, validate_plan};
pub use suggest::{Spot, SuggestOptions, Suggester, Suggestion, suggest};
pub use tiles::MapTiles;
