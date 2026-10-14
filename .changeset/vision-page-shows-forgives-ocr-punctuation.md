**`page shows` on `app: vision` forgives what OCR does to punctuation.**
A greeter page reading `Hello, Ada!` came back from OCR as `Hello Adal`,
so `page shows Hello, Ada` failed against an app that had done exactly the
right thing - and a Citrix stream's compression makes such misreads more
common, not less. On a surface whose text is recognized from pixels, a
positive `shows`/`contains` assertion now also matches with punctuation
stripped from the edges of words and case and spacing ignored. It never
reaches inside a word, so `$10.00` cannot match `$1000` and `1,000` cannot
match `1000`; it never applies to `does not show`, an equality or a count;
and record and replay ask the same question of the driver, so a recording
cannot pass an assertion its replay would fail.
