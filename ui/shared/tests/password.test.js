// Tests for password.js: the same cases as password_advice_lists_the_reasons in crates/accounts/tests/accounts.rs.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { passwordAdvice, REASONS } from '../password.js';

const SHORT = "It's shorter than 12 characters.";
const NAME = "It's close to the account name.";
const COMMON = "It's one of the most common passwords.";
const REPEATS = 'It repeats itself.';

const CASES = [
  ['ann', '', []],
  ['ann', 'short', [SHORT]],
  ['ann', 'elevenchars', [SHORT]],
  ['ann', 'twelve chars', []],
  ['owner', 'owner1', [SHORT, NAME]],
  ['owner', 'owner', [SHORT, NAME]],
  ['mechanic99', 'Mechanic99', [SHORT, NAME]],
  ['mechanic99', 'my-mechanic99-pass', [NAME]],
  ['owner', 'OWNER2024!!x', [NAME]],
  ['owner', 'owner of a red car', []],
  ['ann', 'channel surfing', []],
  ['al', 'alpha', [SHORT]],
  ['longaccountname', 'longaccountname', [NAME]],
  ['ann', 'password1234', [COMMON]],
  ['ann', '123456789012', [COMMON]],
  ['ann', 'qwertyuiop12', [COMMON]],
  ['ann', 'Password123!', [COMMON]],
  ['ann', 'Sentrymate2026', [COMMON]],
  ['ann', 'Tesla1234567', [COMMON]],
  ['ann', 'password', [SHORT, COMMON]],
  ['ann', 'aaaaaaaaaaaa', [REPEATS]],
  ['ann', 'abababababab', [REPEATS]],
  ['ann', 'abcdabcdabcd', [REPEATS]],
  ['ann', '111111111111', [COMMON, REPEATS]],
  ['ann', '  löwe bär fuchs  ', []],
  ['ann', 'correct horse battery', []],
];

test('the same reasons as the server, for the same cases', () => {
  for (const [name, password, want] of CASES) {
    assert.deepEqual(passwordAdvice(name, password), want, `${name} / ${password}`);
  }
});

test('the wording matches the Rust strings', () => {
  assert.deepEqual(Object.values(REASONS), [SHORT, NAME, COMMON, REPEATS]);
});

test('no name, odd input: still a list', () => {
  assert.deepEqual(passwordAdvice(undefined, 'a long enough passphrase'), []);
  assert.deepEqual(passwordAdvice('', 'owner1'), [SHORT]);
  assert.deepEqual(passwordAdvice('ann', null), []);
  // code points count, not UTF-16 units: 12 emoji are 12 characters
  assert.deepEqual(passwordAdvice('ann', 'ab😀cd😀ef😀gh😀'), []);
  assert.deepEqual(passwordAdvice('ann', 'ab😀cd😀ef😀g'), [SHORT]);
});
