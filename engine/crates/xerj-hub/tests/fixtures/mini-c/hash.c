#include "limits.h"

#define MINI_HASH_SEED 5381

/* A tiny djb2 hash, bounded to MINI_LIMIT buckets. */
unsigned long mini_hash(const char *s)
{
    unsigned long h = MINI_HASH_SEED;
    int c;
    while ((c = *s++))
        h = ((h << 5) + h) + (unsigned long)c;
    return h % MINI_LIMIT;
}
