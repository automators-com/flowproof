**Model-authored steps work on `app: vision`.** The authoring model is
told to copy a listed element's token into quotes, and a vision screen
lists nothing but `text:` tokens - yet only `css:` and `id:` were
unwrapped from a quoted label. Every clear, check, select or hover a model
wrote for a vision flow came out as a target named `text:Email`, which
matched nothing on screen, and the step was rejected as naming an element
that was not listed even though it was. `text:` is now unwrapped like the
others.
