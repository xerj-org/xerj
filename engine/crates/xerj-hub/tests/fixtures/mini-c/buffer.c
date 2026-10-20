#include "limits.h"
#include <string.h>

#define MINI_BUFFER_CAP 256

static char mini_buffer[MINI_BUFFER_CAP];

/* Copy at most MINI_BUFFER_CAP-1 bytes, always NUL-terminating. */
int mini_buffer_set(const char *src)
{
    size_t n = strlen(src);
    if (n >= MINI_BUFFER_CAP)
        n = MINI_BUFFER_CAP - 1;
    memcpy(mini_buffer, src, n);
    mini_buffer[n] = '\0';
    return (int)n;
}

const char *mini_buffer_get(void)
{
    return mini_buffer;
}
