/* csil - the C11 reference transport library for the CSIL transport family.
 *
 * Umbrella convenience header: include this to get the whole public API (the
 * conventions, the three transports, and the carrier seam). Consumers may also
 * include the individual headers directly. The canonical-CBOR codec is internal
 * (mirroring the package-private cbor.go in the Go reference) and is not exposed
 * here: hosts encode payloads with generated code, not the transport. */
#ifndef CSIL_H
#define CSIL_H

#include "csil/csil_carrier.h"
#include "csil/csil_conventions.h"
#include "csil/csil_datagrams.h"
#include "csil/csil_events.h"
#include "csil/csil_rpc.h"

#endif /* CSIL_H */
