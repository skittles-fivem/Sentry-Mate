// Parts of the Set-up pages (car settings, display, integrations, server, import, help). A setting page reads
// like the patch bay of a studio desk: numbered channel rows (a label and its explanation on the left, the
// control on the right), a REC lamp in the plate that lights up for a moment after each save, preset buttons
// that jump to a section, and LCD faces for codes, keys and topics.
//
//   const rec = recLamp();                                  // put rec.el in the plate's tools
//   rec.saved('Name');                                      // lights the lamp and says "Name saved."
//   chan({ no: 1, title: 'Name', text: '…', cover }, field.el)
//   presets([['si-broker', 'Broker'], ...])                 // jump buttons
//   toggle('id', on, 'Publish', (on, input) => ...)         // a switch with its label
//   lcd('SMMV-…', { label: 'Move code' })                   // a code on a radio face, in groups
//   copyBtn(() => text)                                     // Copy, with a fallback line

import { h, replace } from '../shared/dom.js';
import { icon, btn } from './ui.js';
import { toast } from './layer.js';
import { errorWords } from './states.js';

/** The words for an error that did not land under a field. */
export const errText = (e) => (e?.fields ? Object.values(e.fields).join(' ') : e?.message || errorWords(e).text);

/** A code-like text: may break after / _ . - (never inside a word). */
export function code(t, { long = false } = {}) {
  const s = String(t ?? '');
  if (!long) return h('code', { class: 'cd' }, s);
  return h('code', { class: 'cd cd--long' }, s.split(/(?<=[/_.-])/).flatMap((part, i) => (i ? [h('wbr'), part] : [part])));
}

/**
 * A channel row: chan({ no, title, text, cover, id, wide }, ...control). `no` is the channel number shown in
 * the small LED box; `wide` puts the control under the words (for tables and long forms).
 */
export function chan({ no, title, text, cover, id, wide = false, tone } = {}, ...control) {
  return h('div', { class: ['chan', wide && 'chan--wide', tone && `chan--${tone}`], cover, id },
    h('div', { class: 'chan-words' },
      h('p', { class: 'chan-head' }, no != null ? h('span', { class: 'chan-no' }, String(no).padStart(2, '0')) : null, h('span', { class: 'chan-title' }, title)),
      text ? h('p', { class: 'chan-text' }, text) : null),
    h('div', { class: 'chan-ctl' }, control));
}

/** A line under a control. */
export const hint = (...text) => h('p', { class: 'chan-hint' }, text);

/**
 * The REC lamp: lights for a moment after a save, with the last thing saved next to it. saved(what) also
 * shows a toast; failed(what, err) shows the error.
 */
export function recLamp({ cover, idle = 'Saves at once' } = {}) {
  const word = h('span', { class: 'rec-word' }, idle);
  const el = h('span', { class: 'rec', role: 'status', 'aria-live': 'polite', cover }, h('span', { class: 'rec-led', 'aria-hidden': 'true' }), word);
  let t = 0;
  const light = (text) => {
    clearTimeout(t);
    el.classList.add('is-on');
    word.textContent = text;
    t = setTimeout(() => { el.classList.remove('is-on'); word.textContent = idle; }, 2400);
  };
  return {
    el,
    saved(what, note) { light(`${what} saved`); toast(note || `${what} saved.`); },
    failed(what, err) { toast(`Could not save ${String(what).toLowerCase()}: ${errText(err)}`, { kind: 'error' }); },
    light,
  };
}

/** Preset buttons that jump to a section of the page: presets([['id', 'Label'], ...]). */
export function presets(list, { label = 'Sections of this page' } = {}) {
  return h('nav', { class: 'presets', 'aria-label': label },
    list.map(([target, text], i) => h('a', { class: 'preset', href: `#${target}`,
      onclick: (e) => {
        const el = document.getElementById(target);
        if (!el) return;
        e.preventDefault();
        const top = document.querySelector('.dash')?.getBoundingClientRect().height || 0;
        const rem = parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
        window.scrollTo({ top: Math.max(0, el.getBoundingClientRect().top + window.scrollY - top - rem), behavior: matchMedia('(prefers-reduced-motion: reduce)').matches ? 'auto' : 'smooth' });
        history.replaceState(history.state, '', `#${target}`);
      } }, h('span', { class: 'preset-n' }, String(i + 1)), h('span', { class: 'preset-t' }, text))));
}

/** A switch with its label: toggle(id, on, label, (on, input) => ..., { disabled }). Returns { el, input }. */
export function toggle(id, checked, label, onChange, { disabled = false, cover } = {}) {
  const input = h('input', { type: 'checkbox', id, role: 'switch', checked: !!checked, disabled, onchange: () => onChange?.(input.checked, input) });
  return { el: h('div', { class: 'tog', cover }, h('span', { class: 'switch' }, input, h('i', { 'aria-hidden': 'true' })), h('label', { class: 'tog-label', for: id }, label)), input };
}

/** A Copy button for a text (or a function giving it), with a line saying what happened. */
export function copyBtn(textOrFn, label = 'Copy', { kind, sm = true } = {}) {
  const said = h('span', { class: 'copy-said', 'aria-live': 'polite' });
  const b = btn(label, async () => {
    const t = typeof textOrFn === 'function' ? textOrFn() : textOrFn;
    try { await navigator.clipboard.writeText(t); said.textContent = 'Copied.'; } catch { said.textContent = 'This browser would not copy: select the text and copy it by hand.'; }
  }, { icon: 'copy', sm, kind });
  return h('span', { class: 'copy' }, b, said);
}

/**
 * A code on a radio face, split into its groups so it wraps between groups only.
 * lcd(text, { label, note, big }) -> element.
 */
export function lcd(text, { label, note, big = false, cover } = {}) {
  const groups = String(text ?? '').split('-');
  return h('div', { class: ['lcd', big && 'lcd--big'], cover },
    label ? h('p', { class: 'lcd-label' }, label) : null,
    h('p', { class: 'lcd-code', 'aria-label': `${label ? `${label}: ` : ''}${text}` }, groups.map((g, i) => h('span', { class: 'lcd-g' }, i ? `-${g}` : g))),
    note ? h('p', { class: 'lcd-note' }, note) : null);
}

/** A signal light with its word: light('ok' | 'warn' | 'bad' | 'off' | 'live', 'Connected'). */
export const sig = (tone, text) => h('span', { class: ['sig', `sig--${tone}`] }, h('span', { class: 'sig-led', 'aria-hidden': 'true' }), h('span', null, text));

/** A check line: the lamp, a head and the text under it, and actions. */
export function checkLine(tone, head, text, actions = null) {
  return h('li', { class: ['chk', `chk--${tone}`] },
    h('span', { class: 'chk-ic' }, icon(tone === 'ok' ? 'check' : tone === 'warn' ? 'warn' : tone === 'bad' ? 'x' : 'info', 'i--sm')),
    h('div', { class: 'chk-body' }, h('p', { class: 'chk-head' }, head), text ? h('p', { class: 'chk-text' }, text) : null, actions ? h('div', { class: 'row chk-acts' }, actions) : null));
}

/** Steps as a row of lamps: steps([[key, label]], atIndex, { failed }). */
export function lampSteps(list, at, { failed = false, done = false, label = 'Steps' } = {}) {
  return h('ol', { class: 'lsteps', 'aria-label': label }, list.map(([, text], i) => {
    const st = done || i < at ? 'done' : i === at ? (failed ? 'bad' : 'now') : 'later';
    return h('li', { class: ['lstep', `lstep--${st}`], 'aria-current': st === 'now' ? 'step' : null },
      h('span', { class: 'lstep-dot' }, st === 'done' ? icon('check', 'i--sm') : st === 'bad' ? icon('x', 'i--sm') : String(i + 1)),
      h('span', { class: 'lstep-t' }, text));
  }));
}

/** Download a text as a file (the notices, a recovery key). */
export function downloadText(filename, text) {
  const href = URL.createObjectURL(new Blob([text], { type: 'text/plain;charset=utf-8' }));
  const a = h('a', { href, download: filename, hidden: true });
  document.body.append(a); a.click(); a.remove();
  setTimeout(() => URL.revokeObjectURL(href), 30000);
}

/** Replace the children of a slot and give it back (for slots painted again after a change). */
export const paint = (slot, ...children) => { replace(slot, ...children); return slot; };
