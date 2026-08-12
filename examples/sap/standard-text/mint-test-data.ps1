$ErrorActionPreference = 'Stop'
$suffix = [DateTime]::UtcNow.ToString('yyMMddHHmmss') + '-' + [Guid]::NewGuid().ToString('N').Substring(0, 6).ToUpperInvariant()
Write-Output "SO10_TEXT_NAME=ZFP-$suffix"
