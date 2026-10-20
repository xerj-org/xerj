#ifndef MINI_LIMITS_H
#define MINI_LIMITS_H

/* The one definition the def-mode test looks up. */
#define MINI_LIMIT 128

#define MINI_VERSION_MAJOR 1
#define MINI_VERSION_MINOR 4
#define MINI_NAME "mini-c"

int mini_clamp(int value, int lo, int hi);

#endif /* MINI_LIMITS_H */
