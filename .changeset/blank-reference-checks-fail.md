**A `${VAR}` that resolves to blank no longer makes a check pass on any page.**
The grammar refuses blank expected text, but a reference got past it, and a
value that arrived empty at run time (`--var ORDER_ID=`, an `env_from` or
DataMaker line with nothing after the `=`, a blank values-file entry) left
`page shows ${ORDER_ID}` checking for blank text, which every page contains.
A data source that returned nothing turned the check into a silent pass.
Positive `shows`, count, `url contains` and `title contains` checks now fail
on record and replay, naming the variable. A `when:` or `repeat: until:`
condition fails the same way in either polarity, rather than taking or
skipping its branch without reading the app.
