#include "BreakoutGame.h"

#include <U8g2lib.h>

namespace {
int clampInt(int value, int minValue, int maxValue) {
  if (value < minValue) {
    return minValue;
  }
  if (value > maxValue) {
    return maxValue;
  }
  return value;
}

float clampFloat(float value, float minValue, float maxValue) {
  if (value < minValue) {
    return minValue;
  }
  if (value > maxValue) {
    return maxValue;
  }
  return value;
}
}  // namespace

BreakoutGame::BreakoutGame(uint32_t width, uint32_t height, uint32_t left)
    : Game("Breakout", width, height), left_(left) {}

void BreakoutGame::onGameReset() {
  paddleX_ = 24.0f;
  paddleDir_ = 1;
  ballX_ = 35.0f;
  ballY_ = 20.0f;
  ballVX_ = 24.0f;
  ballVY_ = -22.0f;
  bricksLeft_ = 0;
  for (uint8_t row = 0; row < BRICK_ROWS; row++) {
    for (uint8_t col = 0; col < BRICK_COLS; col++) {
      bricks_[row][col] = true;
      bricksLeft_++;
    }
  }
}

void BreakoutGame::updateRunning(uint32_t deltaMs, const ButtonInput& input) {
  const float deltaSec = static_cast<float>(deltaMs) * 0.001f;
  const float prevBallX = ballX_;
  const float prevBallY = ballY_;

  if (input.click) {
    paddleDir_ *= -1;
  }

  paddleX_ += static_cast<float>(paddleDir_ * PADDLE_SPEED) * deltaSec;
  paddleX_ = clampFloat(paddleX_, 1.0f, static_cast<float>(width - PADDLE_WIDTH - 1));

  ballX_ += ballVX_ * deltaSec;
  ballY_ += ballVY_ * deltaSec;

  if (ballX_ <= 1.0f) {
    ballX_ = 1.0f;
    ballVX_ = -ballVX_;
  } else if (ballX_ >= static_cast<float>(width - 2)) {
    ballX_ = static_cast<float>(width - 2);
    ballVX_ = -ballVX_;
  }

  if (ballY_ <= 1.0f) {
    ballY_ = 1.0f;
    ballVY_ = -ballVY_;
  }

  const int paddleY = static_cast<int>(height) - 4;
  if (ballY_ >= static_cast<float>(paddleY - 1) &&
      ballY_ <= static_cast<float>(paddleY + 1) &&
      ballX_ >= paddleX_ &&
      ballX_ <= (paddleX_ + static_cast<float>(PADDLE_WIDTH))) {
    ballY_ = static_cast<float>(paddleY - 2);
    ballVY_ = -ballVY_;
  }

  const int ballXi = static_cast<int>(ballX_);
  const int ballYi = static_cast<int>(ballY_);

  bool brickHit = false;
  for (uint8_t row = 0; row < BRICK_ROWS && !brickHit; row++) {
    for (uint8_t col = 0; col < BRICK_COLS; col++) {
      if (!bricks_[row][col]) {
        continue;
      }
      const int bx = 2 + col * BRICK_WIDTH;
      const int by = 2 + row * BRICK_HEIGHT;
      if (ballXi >= bx && ballXi < (bx + BRICK_WIDTH - 1) && ballYi >= by && ballYi < (by + BRICK_HEIGHT - 1)) {
        bricks_[row][col] = false;
        bricksLeft_--;
        brickHit = true;

        const float brickLeft = static_cast<float>(bx);
        const float brickRight = static_cast<float>(bx + BRICK_WIDTH - 1);
        const float brickTop = static_cast<float>(by);
        const float brickBottom = static_cast<float>(by + BRICK_HEIGHT - 1);

        const bool hitFromLeft = prevBallX < brickLeft && ballX_ >= brickLeft;
        const bool hitFromRight = prevBallX > brickRight && ballX_ <= brickRight;
        const bool hitFromTop = prevBallY < brickTop && ballY_ >= brickTop;
        const bool hitFromBottom = prevBallY > brickBottom && ballY_ <= brickBottom;

        if ((hitFromLeft || hitFromRight) && !(hitFromTop || hitFromBottom)) {
          ballVX_ = -ballVX_;
        } else {
          ballVY_ = -ballVY_;
        }
        break;
      }
    }
  }

  if (bricksLeft_ == 0) {
    endGame();
  }

  if (ballY_ >= static_cast<float>(height - 1)) {
    endGame();
  }
}

void BreakoutGame::drawRunning(U8G2& u8g2) {
  u8g2.drawFrame(left_, 0, width, height);
  for (uint8_t row = 0; row < BRICK_ROWS; row++) {
    for (uint8_t col = 0; col < BRICK_COLS; col++) {
      if (bricks_[row][col]) {
        u8g2.drawBox(left_ + 2 + col * BRICK_WIDTH, 2 + row * BRICK_HEIGHT, BRICK_WIDTH - 1, BRICK_HEIGHT - 1);
      }
    }
  }
  const int paddleX = clampInt(static_cast<int>(paddleX_), 1, static_cast<int>(width) - PADDLE_WIDTH - 1);
  const int ballX = clampInt(static_cast<int>(ballX_), 1, static_cast<int>(width) - 2);
  const int ballY = clampInt(static_cast<int>(ballY_), 1, static_cast<int>(height) - 2);
  u8g2.drawBox(left_ + paddleX, height - 4, PADDLE_WIDTH, 2);
  u8g2.drawBox(left_ + ballX, ballY, 2, 2);
}
