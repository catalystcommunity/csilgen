#include "csil/csil_conventions.h"

#include <stdlib.h>

int csil_validate_max_frame(size_t max_frame) {
    return max_frame >= 1 && max_frame <= CSIL_MAX_FRAME_LIMIT;
}

const char *csil_status_name(csil_status s) {
    switch (s.code) {
    case 0:
        return "ok";
    case 1:
        return "malformed-envelope";
    case 2:
        return "unknown-service-or-op";
    case 3:
        return "unauthenticated";
    case 4:
        return "forbidden";
    case 5:
        return "version-unsupported";
    case 6:
        return "internal";
    case 7:
        return "unavailable";
    case 8:
        return "deadline-exceeded";
    default:
        return "other";
    }
}

const char *csil_err_str(csil_err e) {
    switch (e) {
    case CSIL_OK:
        return "ok";
    case CSIL_ERR_OOM:
        return "out of memory";
    case CSIL_ERR_MALFORMED:
        return "malformed envelope";
    case CSIL_ERR_TRUNCATED:
        return "truncated input";
    case CSIL_ERR_TRAILING:
        return "trailing bytes after envelope";
    case CSIL_ERR_UNSUPPORTED_TYPE:
        return "unsupported CBOR item";
    case CSIL_ERR_FRAME_TOO_LARGE:
        return "frame exceeds max-frame guard";
    case CSIL_ERR_UNSUPPORTED_VERSION:
        return "unsupported transport version";
    case CSIL_ERR_CARRIER:
        return "carrier error";
    case CSIL_ERR_INVALID_ARG:
        return "invalid argument";
    default:
        return "unknown error";
    }
}

void csil_free(uint8_t *ptr) { free(ptr); }
