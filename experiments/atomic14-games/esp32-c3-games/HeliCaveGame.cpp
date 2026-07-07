#include "HeliCaveGame.h"

#include <U8g2lib.h>

HeliCaveGame::HeliCaveGame(uint32_t width, uint32_t height, uint32_t left)
    : Game("Heli Cave", width, height), left_(left) {}

void HeliCaveGame::onGameReset() {
  playerY_ = static_cast<float>(height) * 0.5f;
  playerVy_ = 0.0f;
  scrollSpeed_ = 16.0f;
  lastGapTop_ = static_cast<int>(height / 2) - 6;
  randState_ ^= static_cast<uint16_t>(width * 31 + height * 17);

  for (uint8_t i = 0; i < SEGMENT_COUNT; i++) {
    segments_[i].active = false;
  }

  float x = 0.0f;
  for (uint8_t i = 0; i < SEGMENT_COUNT; i++) {
    spawnSegment(x);
    x += SEGMENT_W;
  }
}

void HeliCaveGame::updateRunning(uint32_t deltaMs, const ButtonInput& input) {
  const float deltaSec = static_cast<float>(deltaMs) * 0.001f;
  const float gravity = 44.0f;
  const float lift = 100.0f;
  const float tapImpulse = 10.0f;

  if (input.pressed) {
    playerVy_ -= tapImpulse;
  }
  if (input.down) {
    playerVy_ -= lift * deltaSec;
  } else {
    playerVy_ += gravity * deltaSec;
  }

  if (playerVy_ < -28.0f) {
    playerVy_ = -28.0f;
  } else if (playerVy_ > 22.0f) {
    playerVy_ = 22.0f;
  }

  playerY_ += playerVy_ * deltaSec;
  if (playerY_ < 1.0f) {
    playerY_ = 1.0f;
  }
  if (playerY_ > static_cast<float>(height - PLAYER_H - 1)) {
    playerY_ = static_cast<float>(height - PLAYER_H - 1);
  }

  float rightMostX = -1000.0f;
  for (uint8_t i = 0; i < SEGMENT_COUNT; i++) {
    if (!segments_[i].active) {
      continue;
    }

    segments_[i].x -= scrollSpeed_ * deltaSec;
    if (segments_[i].x > rightMostX) {
      rightMostX = segments_[i].x;
    }

    if ((segments_[i].x + SEGMENT_W) < 0.0f) {
      segments_[i].active = false;
    }
  }

  for (uint8_t i = 0; i < SEGMENT_COUNT; i++) {
    if (!segments_[i].active) {
      spawnSegment(rightMostX + SEGMENT_W);
      if (segments_[i].x > rightMostX) {
        rightMostX = segments_[i].x;
      }
    }
  }

  for (uint8_t i = 0; i < SEGMENT_COUNT; i++) {
    if (segments_[i].active && collidesWithSegment(segments_[i])) {
      endGame();
      return;
    }
  }

  scrollSpeed_ += 0.3f * deltaSec;
  if (scrollSpeed_ > 25.0f) {
    scrollSpeed_ = 25.0f;
  }
}

void HeliCaveGame::drawRunning(U8G2& u8g2) {
  u8g2.drawFrame(left_, 0, width, height);

  for (uint8_t i = 0; i < SEGMENT_COUNT; i++) {
    if (!segments_[i].active) {
      continue;
    }
    const int x = static_cast<int>(segments_[i].x);
    const int topHeight = segments_[i].gapTop;
    const int bottomY = segments_[i].gapTop + segments_[i].gapHeight;
    const int bottomHeight = static_cast<int>(height) - bottomY;

    if (topHeight > 0) {
      u8g2.drawBox(left_ + x, 1, SEGMENT_W, topHeight);
    }
    if (bottomHeight > 0) {
      u8g2.drawBox(left_ + x, bottomY, SEGMENT_W, bottomHeight - 1);
    }
  }

  const int py = static_cast<int>(playerY_);
  u8g2.drawBox(left_ + 12, py, PLAYER_W, PLAYER_H);
  u8g2.drawPixel(left_ + 16, py + 1);
}

uint16_t HeliCaveGame::nextRand() {
  randState_ = static_cast<uint16_t>(randState_ * 2053u + 13849u);
  return randState_;
}

int HeliCaveGame::clampInt(int value, int minValue, int maxValue) const {
  if (value < minValue) {
    return minValue;
  }
  if (value > maxValue) {
    return maxValue;
  }
  return value;
}

int HeliCaveGame::nextGapTop(int previousGapTop) {
  const int minTop = 3;
  const int maxTop = static_cast<int>(height) - static_cast<int>(GAP_HEIGHT) - 3;
  const int delta = static_cast<int>(nextRand() % (MAX_GAP_STEP * 2 + 1)) - MAX_GAP_STEP;
  return clampInt(previousGapTop + delta, minTop, maxTop);
}

void HeliCaveGame::spawnSegment(float x) {
  for (uint8_t i = 0; i < SEGMENT_COUNT; i++) {
    if (!segments_[i].active) {
      lastGapTop_ = nextGapTop(lastGapTop_);
      segments_[i].x = x;
      segments_[i].gapTop = lastGapTop_;
      segments_[i].gapHeight = GAP_HEIGHT;
      segments_[i].active = true;
      return;
    }
  }
}

bool HeliCaveGame::collidesWithSegment(const Segment& segment) const {
  const int playerLeft = 12;
  const int playerTop = static_cast<int>(playerY_);
  const int playerRight = playerLeft + PLAYER_W - 1;
  const int playerBottom = playerTop + PLAYER_H - 1;

  const int segLeft = static_cast<int>(segment.x);
  const int segRight = segLeft + SEGMENT_W - 1;
  if (playerRight < segLeft || playerLeft > segRight) {
    return false;
  }

  const int gapTop = segment.gapTop;
  const int gapBottom = segment.gapTop + segment.gapHeight - 1;
  return (playerTop < gapTop || playerBottom > gapBottom);
}
