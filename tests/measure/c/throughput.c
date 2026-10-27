/* The C side of the event throughput measurement: N key events pushed in
   batches and drained with SDL_PollEvent, reading the fields the windowing
   decoder reads. */
#include <SDL3/SDL.h>
#include <stdio.h>
#include <stdlib.h>

#define BATCH 1000

int main(int argc, char **argv) {
    long count = argc > 1 ? atol(argv[1]) : 1000000;
    if (!SDL_Init(SDL_INIT_VIDEO)) {
        fprintf(stderr, "init: %s\n", SDL_GetError());
        return 1;
    }
    SDL_Window *win = SDL_CreateWindow("measure", 64, 64, 0);
    if (!win) {
        fprintf(stderr, "window: %s\n", SDL_GetError());
        return 1;
    }
    SDL_WindowID id = SDL_GetWindowID(win);
    long drained = 0, repeats = 0;
    Uint64 start = SDL_GetTicksNS();
    while (drained < count) {
        long batch = count - drained < BATCH ? count - drained : BATCH;
        for (long i = 0; i < batch; i++) {
            SDL_Event e;
            SDL_zero(e);
            e.key.type = SDL_EVENT_KEY_DOWN;
            e.key.windowID = id;
            e.key.key = SDLK_A;
            e.key.scancode = SDL_SCANCODE_A;
            e.key.repeat = i % 2 == 0;
            e.key.down = true;
            if (!SDL_PushEvent(&e)) {
                fprintf(stderr, "push: %s\n", SDL_GetError());
                return 1;
            }
        }
        SDL_Event e;
        while (SDL_PollEvent(&e)) {
            if (e.type == SDL_EVENT_KEY_DOWN) {
                drained++;
                repeats += e.key.repeat;
            }
        }
    }
    Uint64 elapsed = SDL_GetTicksNS() - start;
    printf("events %ld repeats %ld ns/event %llu\n", drained, repeats,
           (unsigned long long)(elapsed / (Uint64)drained));
    SDL_DestroyWindow(win);
    SDL_Quit();
    return 0;
}
