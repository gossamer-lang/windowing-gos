# Native session lifetime audit

On Linux X11, this program opens, presents and closes a window in each session,
then finalizes SDL. It checks that the descriptor count after every cycle equals
the count after warmup. `WINDOWING_RESOURCE_CYCLES` selects the repetitions after
warmup, default 32. Other platforms run one presentation and shutdown cycle.

For strict memory checks, build with a system-allocator runtime:

```sh
GOS_RUNTIME_LIB=/absolute/path/libgossamer_runtime.a gos build --release --dynamic tests/resources --out-dir /tmp/windowing-resources
SDL_VIDEO_DRIVER=x11 WINDOWING_RESOURCE_CYCLES=64 GOS_RC_NO_POOL=1 valgrind --leak-check=full --show-leak-kinds=all --errors-for-leak-kinds=definite,indirect,possible --error-exitcode=99 /tmp/windowing-resources/windowing-resource-lifecycle
```

Compare 33 and 65 total sessions. Both must have zero definite, indirect and
possible losses, zero memory errors, and identical remaining live allocation
counts and bytes. No Valgrind suppressions or library preloads are used.

SDL releases display instances, but Xlib also keeps process-wide locale and quark
caches with no public teardown API. The X11 backend preserves the already loaded
library's static owners using `RTLD_NOLOAD | RTLD_NODELETE`, then balances the
temporary `dlopen` reference with `dlclose`. This is scoped to the active X11
driver. [The loader contract](https://man7.org/linux/man-pages/man3/dlopen.3.html)
describes promotion of flags on an existing library and process-lifetime storage.
The repeated-session check ensures those retained caches do not grow per session.
