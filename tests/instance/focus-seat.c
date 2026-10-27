/* Focus only the nested compositor on the fixture's private X server. */
#include <X11/Xlib.h>
#include <stdlib.h>
#include <stdio.h>
#include <X11/Xatom.h>
int main(int argc, char **argv) {
    if (argc != 2) return 2;
    Display *display = XOpenDisplay(NULL);
    if (!display) return 3;
    Window window = strtoul(argv[1], NULL, 10);
    if (!window) {
        Window root, parent, *children = NULL;
        unsigned int count = 0;
        if (!XQueryTree(display, DefaultRootWindow(display), &root, &parent, &children, &count)) return 4;
        for (unsigned int index = 0; index < count; ++index) {
            XWindowAttributes attrs;
            if (XGetWindowAttributes(display, children[index], &attrs) && attrs.map_state == IsViewable) { window = children[index]; break; }
        }
        if (children) XFree(children);
        if (!window) return 5;
    }
    XSetInputFocus(display, window, RevertToPointerRoot, CurrentTime);
    XWarpPointer(display, None, window, 0, 0, 0, 0, 400, 300);
    XSync(display, False);
    Window clock = XCreateSimpleWindow(display, DefaultRootWindow(display), 0, 0, 1, 1, 0, 0, 0);
    XSelectInput(display, clock, PropertyChangeMask);
    unsigned char value = 1;
    XChangeProperty(display, clock, XA_WM_NAME, XA_STRING, 8, PropModeReplace, &value, 1);
    XEvent event;
    XWindowEvent(display, clock, PropertyChangeMask, &event);
    printf("%lu\n", event.xproperty.time);
    XDestroyWindow(display, clock);
    XCloseDisplay(display);
    return 0;
}
