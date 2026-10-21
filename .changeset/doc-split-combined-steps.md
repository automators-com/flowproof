- **A document step that holds several actions drafts as several steps.**
  ALM steps often read "select the plant, then click Save". `author-from-doc`
  flagged such a step whole whenever one of its actions was vague, so the
  concrete "Press the "Save" button" was lost in the flag too. Each action is
  now judged on its own and drafted in order: the concrete ones as real steps,
  only the vague one flagged. The Expected outcome still follows the step's
  last action. Splitting doesn't license inventing: an action that needs a
  URL, path, label or value the document doesn't give is flagged, not filled
  in with a guess.
