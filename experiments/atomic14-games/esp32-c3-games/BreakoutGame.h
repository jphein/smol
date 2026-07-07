#pragma once

#include "Game.h"

class BreakoutGame : public Game {
  public:
    BreakoutGame(uint32_t width, uint32_t height, uint32_t left);

  protected:
    void onGameReset() override;
    void updateRunning(uint32_t deltaMs, const ButtonInput& input) override;
    void drawRunning(U8G2& u8g2) override;

  private:
    static constexpr uint8_t PADDLE_WIDTH = 14;
    static constexpr int PADDLE_SPEED = 32;
    static constexpr uint8_t BRICK_ROWS = 4;
    static constexpr uint8_t BRICK_COLS = 8;
    static constexpr uint8_t BRICK_WIDTH = 8;
    static constexpr uint8_t BRICK_HEIGHT = 4;

    uint32_t left_;
    float paddleX_ = 24.0f;
    int paddleDir_ = 1;
    float ballX_ = 35.0f;
    float ballY_ = 20.0f;
    float ballVX_ = 24.0f;
    float ballVY_ = -22.0f;
    int bricksLeft_ = 0;
    bool bricks_[BRICK_ROWS][BRICK_COLS] = {};
};
