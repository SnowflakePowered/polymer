#include <SDL3/SDL.h>
#include <stdio.h>
#include <string.h>

int main(void) {
    if (!SDL_Init(SDL_INIT_GAMEPAD)) return 1;
    SDL_Gamepad *pad = NULL;
    Uint64 deadline = SDL_GetTicks() + 10000;
    while (SDL_GetTicks() < deadline) {
        int count = 0;
        SDL_JoystickID *ids = SDL_GetGamepads(&count);
        if (!pad && count == 1) pad = SDL_OpenGamepad(ids[0]);
        SDL_free(ids);
        if (pad) {
            if (strcmp(SDL_GetGamepadName(pad), "Polymer Virtual Gamepad")) return 2;
            SDL_UpdateGamepads();
            SDL_Joystick *joystick = SDL_GetGamepadJoystick(pad);
            if (SDL_GetJoystickButton(joystick, 0) &&
                SDL_GetJoystickAxis(joystick, 6) > 0 &&
                SDL_GetJoystickAxis(joystick, 7) < 0) {
                if (!SDL_RumbleGamepad(pad, 123, 456, 70)) return 3;
                // Give the receiver thread time to deliver feedback before shutdown.
                SDL_Delay(100);
                SDL_CloseGamepad(pad);
                SDL_Quit();
                puts("virtual controller received buttons and mouse axes and sent rumble");
                return 0;
            }
        }
        SDL_Delay(10);
    }
    fprintf(stderr, "No virtual controller input: %s\n", SDL_GetError());
    return 4;
}
