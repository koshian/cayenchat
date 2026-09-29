param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string] $Executable
)

$stream = [System.IO.File]::OpenRead((Resolve-Path $Executable))
$reader = [System.IO.BinaryReader]::new($stream)
try {
    $stream.Position = 0x3c
    $peOffset = $reader.ReadInt32()
    $stream.Position = $peOffset
    if ($reader.ReadUInt32() -ne 0x00004550) {
        throw "Not a PE executable: $Executable"
    }

    # PE signature (4 bytes), COFF header (20 bytes), then optional header.
    $stream.Position = $peOffset + 24 + 68
    $subsystem = $reader.ReadUInt16()
    if ($subsystem -ne 2) {
        throw "Expected Windows GUI subsystem (2), found $subsystem in $Executable"
    }

    Write-Host "Verified Windows GUI subsystem in $Executable"
}
finally {
    $reader.Dispose()
    $stream.Dispose()
}
