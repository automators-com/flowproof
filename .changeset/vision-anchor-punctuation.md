**A vision label ending in a colon is found when OCR drops the colon.** OCR
read the label `Describe:` as `Describe`, and since an anchor had to match its
text exactly - case aside - the field could not be found anywhere on the page.
Labels that end in a colon are the norm on SAP screens and business forms. As
a last resort after an exact and a case-insensitive match, an anchor now also
matches with punctuation at the edges of words ignored; an exact match still
wins, and punctuation inside a word still counts.
