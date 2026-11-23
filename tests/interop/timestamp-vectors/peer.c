/* Decode every timestamp vector as a Stamp, then re-encode the rows that must
 * decode. The C CsilTimestamp keeps canonical text, so it holds nine digits. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "types.gen.h"
#include "codec.gen.h"

enum { DIGITS = 9 };

static size_t unhex(const char *s, uint8_t *out) {
    size_t n = strlen(s) / 2;
    for (size_t i = 0; i < n; i++) {
        unsigned int byte;
        if (sscanf(s + 2 * i, "%2x", &byte) != 1) return 0;
        out[i] = (uint8_t)byte;
    }
    return n;
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
    char line[1024];
    int failures = 0;
    while (fgets(line, sizeof line, f)) {
        line[strcspn(line, "\r\n")] = 0;
        if (line[0] == 0 || line[0] == '#') continue;
        char *fields[5];
        char *cursor = line;
        for (int i = 0; i < 5; i++) {
            fields[i] = cursor;
            cursor = strchr(cursor, '\t');
            if (cursor) *cursor++ = 0;
            else if (i < 4) {
                fprintf(stderr, "malformed row: %s\n", fields[0]);
                return 2;
            }
        }
        const char *name = fields[0];
        int want_ok = strcmp(fields[1], "ok") == 0 && atoi(fields[2]) <= DIGITS;
        uint8_t input[512];
        size_t input_len = unhex(fields[3], input);
        Stamp stamp;
        CsilCodecArena *owner = NULL;
        int decoded = csil_decode_Stamp(input, input_len, &stamp, &owner) == 0;
        if (want_ok && !decoded) {
            printf("%s: decode failed\n", name);
            failures++;
        } else if (want_ok) {
            uint8_t *out = NULL;
            size_t out_len = 0;
            if (csil_encode_Stamp(&stamp, &out, &out_len) != 0) {
                printf("%s: encode failed\n", name);
                failures++;
            } else {
                char got[1024];
                for (size_t i = 0; i < out_len; i++) sprintf(got + 2 * i, "%02x", out[i]);
                got[2 * out_len] = 0;
                if (strcmp(got, fields[4]) != 0) {
                    printf("%s: encoded %s\n", name, got);
                    failures++;
                }
                free(out);
            }
        } else if (decoded) {
            printf("%s: decode accepted %s\n", name, stamp.at.rfc3339);
            failures++;
        }
        if (owner) csil_codec_arena_free(owner);
    }
    fclose(f);
    if (failures) return 1;
    printf("c: all timestamp vectors pass\n");
    return 0;
}
