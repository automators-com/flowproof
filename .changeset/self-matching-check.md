**A check that finds its element by the very text it expects is flagged.**
`assert: Verify that the calculator displays "8"` on `app: vision` was
recorded as "the element reading 8 shows 8". That always holds, so on replay
it passed with the display showing 5, by matching the keypad's 8 key. It was
a false green that nothing reported. `record` now warns on such a step
(`WARNING: step N: … it cannot fail`, and as the step's `warning` in
`--json`), the same way it already warns about a `page shows` that held
before the steps it follows. It is a warning, not a refusal: check a value
no key or label on the screen also shows, or name the element some other
way.
