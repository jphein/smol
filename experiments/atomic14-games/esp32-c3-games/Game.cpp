#include "Game.h"

SingleButton::SingleButton(uint16_t debounceMs, uint16_t longPressMs)
    : debounceMs_(debounceMs), longPressMs_(longPressMs) {}

void SingleButton::reset(bool rawDown, uint32_t nowMs) {
    rawDown_ = rawDown;
    debouncedDown_ = rawDown;
    longPressEmitted_ = false;
    rawChangedAtMs_ = nowMs;
    pressedAtMs_ = nowMs;
}

ButtonInput SingleButton::update(bool rawDown, uint32_t nowMs) {
    ButtonInput input;

    if (rawDown != rawDown_) {
        rawDown_ = rawDown;
        rawChangedAtMs_ = nowMs;
    }

    if ((nowMs - rawChangedAtMs_) >= debounceMs_ && debouncedDown_ != rawDown_) {
        debouncedDown_ = rawDown_;
        if (debouncedDown_) {
            input.pressed = true;
            pressedAtMs_ = nowMs;
            longPressEmitted_ = false;
        } else {
            input.released = true;
            if (!longPressEmitted_) {
                input.click = true;
            }
        }
    }

    if (debouncedDown_) {
        input.holdMs = nowMs - pressedAtMs_;
        if (!longPressEmitted_ && input.holdMs >= longPressMs_) {
            longPressEmitted_ = true;
            input.longPress = true;
        }
    }

    input.down = debouncedDown_;
    return input;
}

Game::Game(const char* title, uint32_t width, uint32_t height)
    : title(title), width(width), height(height) {}

void Game::begin(uint32_t nowMs, bool buttonDown) {
    button_.reset(buttonDown, nowMs);
    phase_ = GamePhase::Start;
    lastUpdateMs_ = nowMs;
    gameOver_ = false;
    exitToMenuRequested_ = false;
}

void Game::tick(uint32_t nowMs, bool buttonDown) {
    const ButtonInput input = button_.update(buttonDown, nowMs);
    const uint32_t deltaMs = nowMs - lastUpdateMs_;
    lastUpdateMs_ = nowMs;

    switch (phase_) {
        case GamePhase::Start:
            if (input.click) {
                startRunning();
            }
            break;
        case GamePhase::Running:
            updateRunning(deltaMs, input);
            if (gameOver_) {
                phase_ = GamePhase::End;
            }
            break;
        case GamePhase::End:
            if (input.longPress) {
                exitToMenuRequested_ = true;
                phase_ = GamePhase::Start;
            } else if (input.click) {
                startRunning();
            }
            break;
    }
}

void Game::render(U8G2& u8g2) {
    switch (phase_) {
        case GamePhase::Start:
            drawStart(u8g2);
            break;
        case GamePhase::Running:
            drawRunning(u8g2);
            break;
        case GamePhase::End:
            drawEnd(u8g2);
            break;
    }
}

bool Game::shouldExitToMenu() const {
    return exitToMenuRequested_;
}

void Game::clearExitRequest() {
    exitToMenuRequested_ = false;
}

GamePhase Game::phase() const {
    return phase_;
}

const char* Game::gameTitle() const {
    return title;
}

void Game::endGame() {
    gameOver_ = true;
}

void Game::onGameReset() {}

void Game::drawStart(U8G2& u8g2) {
    (void)u8g2;
}

void Game::drawEnd(U8G2& u8g2) {
    (void)u8g2;
}

void Game::startRunning() {
    gameOver_ = false;
    onGameReset();
    phase_ = GamePhase::Running;
}
