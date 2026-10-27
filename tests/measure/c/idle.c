/* The C side of the idle measurement: one window, a presented frame (or a
   GL context), then SDL_WaitEvent until quit. Same SDL build, same calls
   the windowing library makes. */
#include <SDL3/SDL.h>
#include <stdio.h>
#include <string.h>

int main(int argc, char **argv) {
    int gl = argc > 1 && strcmp(argv[1], "gl") == 0;
    if (!SDL_Init(SDL_INIT_VIDEO)) {
        fprintf(stderr, "init: %s\n", SDL_GetError());
        return 1;
    }
    SDL_Window *win = SDL_CreateWindow("measure", 640, 480, gl ? SDL_WINDOW_OPENGL : 0);
    if (!win) {
        fprintf(stderr, "window: %s\n", SDL_GetError());
        return 1;
    }
    SDL_GLContext ctx = NULL;
    if (gl) {
        ctx = SDL_GL_CreateContext(win);
        if (!ctx) {
            fprintf(stderr, "gl: %s\n", SDL_GetError());
            return 1;
        }
        SDL_GL_SwapWindow(win);
    } else {
        SDL_Surface *s = SDL_GetWindowSurface(win);
        SDL_FillSurfaceRect(s, NULL, SDL_MapSurfaceRGB(s, 30, 30, 30));
        SDL_UpdateWindowSurface(win);
    }
    printf("ready\n");
    fflush(stdout);
    SDL_Event e;
    while (SDL_WaitEvent(&e)) {
        if (e.type == SDL_EVENT_QUIT || e.type == SDL_EVENT_WINDOW_CLOSE_REQUESTED) {
            break;
        }
    }
    if (ctx) {
        SDL_GL_DestroyContext(ctx);
    }
    SDL_DestroyWindow(win);
    SDL_Quit();
    return 0;
}
