// The UI packs a person can choose, for the "UI pack" picker of Settings: Display and units.
//
// GET /api/v1/ui-packs lists every built-in and uploaded pack for anyone signed in. A pack made for another
// API version (api_supported false) can't be chosen, so it is left out, unless it is the person's choice now.
// When the request fails the page's own short list is used instead, so the picker always works.

// The first phrase of a pack's description, short enough for a picker row: "Soft, friendly and touch first".
export function packBlurb(description, max = 72) {
  if (typeof description !== 'string') return '';
  let s = description.trim().split(/(?<=[.!?])\s/)[0].split(/:\s/)[0].replace(/[.!?]$/, '').trim();
  if (s.length > max) s = `${s.slice(0, max - 1).replace(/[\s,;]+\S*$/, '')}…`;
  return s;
}

// [{ id, name, blurb, version }] for the picker, the current pack among them, and whether the list came from the
// server. `fallback` is [[id, name, blurb?], ...]; `current` the person's ui_pack (or the pack being served).
export async function packChoices(client, { fallback = [], current = null } = {}) {
  let items = null;
  try {
    const r = await client.get('ui-packs');
    if (r && Array.isArray(r.items)) {
      items = r.items
        .filter((p) => p && typeof p.id === 'string' && (p.api_supported !== false || p.id === current))
        .map((p) => ({ id: p.id, name: p.name || p.id, blurb: packBlurb(p.description), version: p.version || '' }));
    }
  } catch { items = null; }
  const fromServer = !!items && items.length > 0;
  if (!fromServer) items = fallback.map(([id, name, blurb]) => ({ id, name: name || id, blurb: blurb || '', version: '' }));
  if (current && !items.some((p) => p.id === current)) items.push({ id: current, name: current, blurb: '', version: '' });
  return { items, fromServer };
}

// "Pebble: Soft, friendly and touch first", for a plain <option>.
export const packLabel = (p) => (p.blurb ? `${p.name}: ${p.blurb}` : p.name);
