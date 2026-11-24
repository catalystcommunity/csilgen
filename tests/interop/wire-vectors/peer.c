/* Decode every wire vector as the type its kind names, then re-encode the rows
 * that must decode and compare with the canonical bytes. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "types.gen.h"
#include "codec.gen.h"

static size_t unhex(const char *s, uint8_t *out) {
    size_t n = strlen(s) / 2;
    for (size_t i = 0; i < n; i++) {
        unsigned int byte;
        if (sscanf(s + 2 * i, "%2x", &byte) != 1) return 0;
        out[i] = (uint8_t)byte;
    }
    return n;
}

/* Decode the input as one type; on success re-encode it into *out. Returns 1 when
 * the decode failed, 2 when the encode failed, 0 on success. */
#define ROUND_TRIP(T, in, n, out, out_len)                                         \
    do {                                                                           \
        T value;                                                                   \
        CsilCodecArena *owner = NULL;                                              \
        if (csil_decode_##T(in, n, &value, &owner) != 0) return 1;                 \
        int rc = csil_encode_##T(&value, out, out_len) != 0 ? 2 : 0;               \
        csil_codec_arena_free(owner);                                              \
        return rc;                                                                 \
    } while (0)

static int round_trip(const char *kind, const uint8_t *in, size_t n, uint8_t **out,
                      size_t *out_len) {
    if (strcmp(kind, "holder") == 0) ROUND_TRIP(Holder, in, n, out, out_len);
    if (strcmp(kind, "contact") == 0) ROUND_TRIP(Contact, in, n, out, out_len);
    if (strcmp(kind, "output") == 0) ROUND_TRIP(LookupReadCountsOutput, in, n, out, out_len);
    return 3;
}

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: peer vectors.tsv\n");
        return 2;
    }
    FILE *f = fopen(argv[1], "r");
    if (!f) {
        perror(argv[1]);
        return 2;
    }
    char line[2048];
    int failures = 0;
    while (fgets(line, sizeof line, f)) {
        line[strcspn(line, "\r\n")] = 0;
        if (line[0] == 0 || line[0] == '#') continue;
        char *fields[5];
        char *cursor = line;
        for (int i = 0; i < 5; i++) {
            fields[i] = cursor;
            cursor = cursor ? strchr(cursor, '\t') : NULL;
            if (cursor) *cursor++ = 0;
            else if (i < 4) {
                fprintf(stderr, "malformed row: %s\n", fields[0]);
                return 2;
            }
        }
        const char *name = fields[0];
        int want_ok = strcmp(fields[2], "ok") == 0;
        uint8_t input[1024];
        size_t input_len = unhex(fields[3], input);
        uint8_t *out = NULL;
        size_t out_len = 0;
        int rc = round_trip(fields[1], input, input_len, &out, &out_len);
        if (rc == 3) {
            printf("FAIL %s: unknown kind %s\n", name, fields[1]);
            failures++;
        } else if (want_ok && rc == 1) {
            printf("FAIL %s: decode failed\n", name);
            failures++;
        } else if (want_ok && rc == 2) {
            printf("FAIL %s: encode failed\n", name);
            failures++;
        } else if (want_ok) {
            const char *canonical = strcmp(fields[4], "-") == 0 ? fields[3] : fields[4];
            char got[2048];
            for (size_t i = 0; i < out_len; i++) sprintf(got + 2 * i, "%02x", out[i]);
            got[2 * out_len] = 0;
            if (strcmp(got, canonical) != 0) {
                printf("FAIL %s: encoded %s, want %s\n", name, got, canonical);
                failures++;
            }
        } else if (rc != 1) {
            printf("FAIL %s: decode accepted an invalid input\n", name);
            failures++;
        }
        free(out);
    }
    fclose(f);
    if (failures) return 1;
    printf("all wire vectors pass\n");
    return 0;
}
