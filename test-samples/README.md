# Test samples

Paste these into Notepad (or any text field) to exercise Alfred Writer at different
lengths, styles, and error densities. All contain deliberate grammar/spelling mistakes.

- `01-short-casual.txt` — a few sentences, informal tone, several typos ("apreciate", "ment alot"). Good for a quick single-check smoke test.
- `02-medium-email.txt` — a business email, paragraph-length, mix of subject-verb agreement and homophone errors (their/they're/there, wether).
- `03-long-essay.txt` — several paragraphs of prose. Good for testing checks on longer text and the debounce/cooldown behavior as you keep editing.
- `04-technical-software.txt` — dense engineering jargon (backoff, circuit breakers, thundering herd) with errors mixed into technical sentences — checks the model doesn't get confused by domain terms it shouldn't "correct".
- `05-technical-scientific.txt` — academic/biology register, longer sentences, passive voice — a harder case for both detection and for multi-word phrase apply (longer "original" spans).
- `06-tiny-edge-case.txt` — deliberately short (under the 12-character-after-trim minimum once you delete a bit); use it to confirm the popup does *not* fire on very short text.

Tip: paste one in, then edit a word or two and leave it alone — you should see exactly
one `claude` check fire per meaningful pause (not per keystroke), and switching away and
back without changing anything should not trigger another one.
