#pragma once

#include <stdint.h>

class U8G2;

struct ButtonInput {
    bool down = false;
    bool pressed = false;
    bool released = false;
    bool click = false;
    bool longPress = false;
    uint32_t holdMs = 0;
};

class SingleButton {
    public:
        SingleButton(uint16_t debounceMs = 30, uint16_t longPressMs = 700);

        void reset(bool rawDown, uint32_t nowMs);
        ButtonInput update(bool rawDown, uint32_t nowMs);

    private:
        uint16_t debounceMs_;
        uint16_t longPressMs_;
        bool rawDown_ = false;
        bool debouncedDown_ = false;
        bool longPressEmitted_ = false;
        uint32_t rawChangedAtMs_ = 0;
        uint32_t pressedAtMs_ = 0;
};

enum class GamePhase {
    Start,
    Running,
    End
};

class Game {
    protected:
        const char* title;
        uint32_t width;
        uint32_t height;

        void endGame();

    public:
        Game(const char* title, uint32_t width, uint32_t height);
        virtual ~Game() = default;

        void begin(uint32_t nowMs, bool buttonDown);
        void tick(uint32_t nowMs, bool buttonDown);
        void render(U8G2& u8g2);

        bool shouldExitToMenu() const;
        void clearExitRequest();
        GamePhase phase() const;
        const char* gameTitle() const;

    protected:
        virtual void onGameReset();
        virtual void updateRunning(uint32_t deltaMs, const ButtonInput& input) = 0;
        virtual void drawRunning(U8G2& u8g2) = 0;
        virtual void drawStart(U8G2& u8g2);
        virtual void drawEnd(U8G2& u8g2);

    private:
        void startRunning();

        GamePhase phase_ = GamePhase::Start;
        SingleButton button_;
        uint32_t lastUpdateMs_ = 0;
        bool gameOver_ = false;
        bool exitToMenuRequested_ = false;
};