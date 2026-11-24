// Advice on a new password, the same as the server's password_advice (crates/accounts/src/password.rs): the same
// reasons in the same words. The server refuses only an empty password and one over 1,024 bytes; a weak one is the
// person's choice. Pages show these reasons before the form is sent and let the person use the password anyway.

/** Characters a password should have besides the account name. */
export const ADVISED_CHARS = 12;

export const REASONS = Object.freeze({
  short: "It's shorter than 12 characters.",
  name: "It's close to the account name.",
  common: "It's one of the most common passwords.",
  repeats: "It repeats itself.",
});

// Passwords too common to be safe even at 12 characters or more (compared ignoring case).
const COMMON = new Set([
  '123456789012', '1234567890123', '12345678901234', '123456789123', '111111111111', '000000000000', '123123123123',
  '121212121212', 'qwertyuiop12', 'qwertyuiop123', 'qwertyuiopasdfgh', '1q2w3e4r5t6y', '1qaz2wsx3edc',
  'q1w2e3r4t5y6', 'zaq12wsxcde3', 'asdfghjkl123', 'qazwsxedcrfv', 'passwordpassword', 'password1234',
  'password12345', 'password123456', 'passw0rd1234', 'p@ssw0rd1234', 'p@ssword1234', 'iloveyou1234', 'letmein12345',
  'welcome12345', 'administrator', 'administrator1', 'changeme1234', 'trustno11234', 'football1234', 'baseball1234',
  'superman1234', 'princess1234', 'sunshine1234', 'starwars1234', 'whatever1234', 'monkey123456', 'dragon123456',
  'abcdefghijkl', 'abcd1234abcd', 'abc123abc123', 'qwerty123456', 'qwertyqwerty', 'asdfasdfasdf',
  'correcthorsebatterystaple', 'sentrymate123', 'sentrymate1234', 'teslamodel3y', 'teslamodely1', 'teslamodel31',
  'tesla1234567', 'teslatesla12',
]);

// Words that, followed only by digits and symbols, make a password easy to guess ("Password123!").
const COMMON_WORDS = new Set([
  'password', 'passw0rd', 'p@ssw0rd', 'p@ssword', 'qwerty', 'qwertyuiop', 'letmein', 'welcome', 'admin',
  'administrator', 'iloveyou', 'changeme', 'secret', 'sentrymate', 'sentry', 'screenmate', 'teslamate', 'tesla',
  'monkey', 'dragon', 'football', 'baseball', 'sunshine', 'princess',
]);

// Code points, so 'bär' is three characters like in Rust.
const chars = (s) => Array.from(s);

// One short piece (up to 4 characters) over and over: 'aaaaaaaaaaaa', 'abababababab'.
function repeats(c) {
  for (let unit = 1; unit <= 4; unit++) {
    if (c.length <= unit || c.length % unit !== 0) continue;
    let same = true;
    for (let i = unit; i < c.length && same; i++) same = c[i] === c[i % unit];
    if (same) return true;
  }
  return false;
}

// ASCII digits, ASCII punctuation and spaces at the end, as Rust's trim_end_matches does.
const TAIL = /[0-9!-\/:-@\[-`{-~ ]+$/;

/**
 * Why a password would be easy to guess, as short sentences; an empty list when there is nothing to say.
 * Spaces count and nothing is trimmed: a passphrase is welcome.
 */
export function passwordAdvice(name, password) {
  const why = [];
  if (typeof password !== 'string' || password === '') return why;
  const lower = password.toLowerCase();
  const c = chars(lower);
  const n = String(name ?? '').trim().toLowerCase();
  const close = n !== '' && (lower === n
    || (chars(n).length >= 3 && lower.includes(n) && chars(lower.split(n).join('')).length < ADVISED_CHARS));
  if (c.length < ADVISED_CHARS) why.push(REASONS.short);
  if (close) why.push(REASONS.name);
  const stripped = lower.replace(TAIL, '');
  if (COMMON.has(lower) || COMMON_WORDS.has(stripped)) why.push(REASONS.common);
  if (repeats(c)) why.push(REASONS.repeats);
  return why;
}
