module csilinterop

go 1.26.3

require (
	github.com/catalystcommunity/csilgen/transports/go v0.0.0
	interopapi v0.0.0
)

replace interopapi => ./gen

replace github.com/catalystcommunity/csilgen/transports/go => ../../../../transports/go
