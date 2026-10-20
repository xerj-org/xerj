#include "limits.h"
#include <ctype.h>

#define MINI_RADIX 10

/* Parse a non-negative decimal integer, capped at MINI_LIMIT. */
int mini_parse_uint(const char *s)
{
    int acc = 0;
    while (*s && isdigit((unsigned char)*s)) {
        acc = acc * MINI_RADIX + (*s - '0');
        if (acc > MINI_LIMIT)
            return MINI_LIMIT;
        s++;
    }
    return acc;
}
