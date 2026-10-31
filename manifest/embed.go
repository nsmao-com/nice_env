package manifest

import "embed"

//go:embed packages.win.json packages.mac.json
var Files embed.FS
