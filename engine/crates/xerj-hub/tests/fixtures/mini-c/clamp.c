#include "limits.h"

/* Clamp a value into [lo, hi]. */
int mini_clamp(int value, int lo, int hi)
{
    if (value < lo)
        return lo;
    if (value > hi)
        return hi;
    return value;
}

int mini_within_limit(int value)
{
    return mini_clamp(value, 0, MINI_LIMIT);
}
