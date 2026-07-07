#include "JumpGame.h"

#include <U8g2lib.h>

JumpGame::JumpGame(uint32_t width, uint32_t height, uint32_t left)
    : Game("Jump Run", width, height), left_(left) {}

void JumpGame::onGameReset() {
  playerYPos_ = static_cast<float>(groundY() - PLAYER_H);
  playerVy_ = 0.0f;
  obstacleSpeed_ = 15.0f;
  spawnTimerMs_ = 0;
  nextSpawnMs_ = 1250;
  spawnPattern_ = 0;
  survivedTicks_ = 0;
  for (uint8_t i = 0; i < OBSTACLE_COUNT; i++) {
    obstacles_[i].active = false;
  }
}

void JumpGame::updateRunning(uint32_t deltaMs, const ButtonInput& input) {
  const float deltaSec = static_cast<float>(deltaMs) * 0.001f;
  const float gravity = 72.0f;
  const float jumpVelocity = -42.0f;

  if (input.click && isOnGround()) {
    playerVy_ = jumpVelocity;
  }

  playerVy_ += gravity * deltaSec;
  playerYPos_ += playerVy_ * deltaSec;

  const float floorY = static_cast<float>(groundY() - PLAYER_H);
  if (playerYPos_ >= floorY) {
    playerYPos_ = floorY;
    playerVy_ = 0.0f;
  }

  spawnTimerMs_ += deltaMs;
  if (spawnTimerMs_ >= nextSpawnMs_) {
    spawnTimerMs_ = 0;
    spawnObstacle();
    nextSpawnMs_ = static_cast<uint16_t>(1180 + ((spawnPattern_ % 4) * 180));
  }

  for (uint8_t i = 0; i < OBSTACLE_COUNT; i++) {
    if (!obstacles_[i].active) {
      continue;
    }
    obstacles_[i].x -= obstacleSpeed_ * deltaSec;
    if ((obstacles_[i].x + obstacles_[i].w) < 0.0f) {
      obstacles_[i].active = false;
      survivedTicks_++;
      if ((survivedTicks_ % 8) == 0 && obstacleSpeed_ < 24.0f) {
        obstacleSpeed_ += 1.0f;
      }
      continue;
    }
    if (intersectsObstacle(obstacles_[i])) {
      endGame();
      return;
    }
  }
}

void JumpGame::drawRunning(U8G2& u8g2) {
  u8g2.drawFrame(left_, 0, width, height);
  u8g2.drawLine(left_ + 1, groundY(), left_ + width - 2, groundY());

  for (uint8_t i = 0; i < OBSTACLE_COUNT; i++) {
    if (!obstacles_[i].active) {
      continue;
    }
    u8g2.drawBox(
        left_ + static_cast<int>(obstacles_[i].x),
        groundY() - obstacles_[i].h,
        obstacles_[i].w,
        obstacles_[i].h);
  }

  u8g2.drawBox(left_ + PLAYER_X, playerY(), PLAYER_W, PLAYER_H);
}

int JumpGame::groundY() const {
  return static_cast<int>(height) - GROUND_MARGIN;
}

int JumpGame::playerY() const {
  return static_cast<int>(playerYPos_);
}

bool JumpGame::isOnGround() const {
  return playerYPos_ >= static_cast<float>(groundY() - PLAYER_H - 0.1f);
}

void JumpGame::spawnObstacle() {
  // Keep a minimum horizontal gap so obstacles are readable and fair.
  const int minGapPx = 28;
  float rightMostX = -1000.0f;
  bool hasActiveObstacle = false;

  for (uint8_t i = 0; i < OBSTACLE_COUNT; i++) {
    if (!obstacles_[i].active) {
      continue;
    }
    hasActiveObstacle = true;
    if (obstacles_[i].x > rightMostX) {
      rightMostX = obstacles_[i].x;
    }
  }

  if (hasActiveObstacle && rightMostX > static_cast<float>(width - minGapPx)) {
    return;
  }

  for (uint8_t i = 0; i < OBSTACLE_COUNT; i++) {
    if (!obstacles_[i].active) {
      obstacles_[i].active = true;
      obstacles_[i].x = static_cast<float>(width - 4);
      obstacles_[i].w = 3 + (spawnPattern_ % 2);
      obstacles_[i].h = 4 + ((spawnPattern_ * 2) % 4);
      spawnPattern_++;
      return;
    }
  }
}

bool JumpGame::intersectsObstacle(const Obstacle& obstacle) const {
  const int playerLeft = PLAYER_X;
  const int playerTop = playerY();
  const int playerRight = PLAYER_X + PLAYER_W - 1;
  const int playerBottom = playerTop + PLAYER_H - 1;

  const int obstacleLeft = static_cast<int>(obstacle.x);
  const int obstacleTop = groundY() - obstacle.h;
  const int obstacleRight = obstacleLeft + obstacle.w - 1;
  const int obstacleBottom = groundY() - 1;

  if (playerRight < obstacleLeft || playerLeft > obstacleRight) {
    return false;
  }
  if (playerBottom < obstacleTop || playerTop > obstacleBottom) {
    return false;
  }
  return true;
}
