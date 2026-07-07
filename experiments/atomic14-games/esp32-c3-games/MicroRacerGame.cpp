#include "MicroRacerGame.h"

#include <Arduino.h>
#include <U8g2lib.h>

MicroRacerGame::MicroRacerGame(uint32_t width, uint32_t height, uint32_t left)
    : Game("Micro Racer", width, height), left_(left) {}

void MicroRacerGame::onGameReset() {
  playerLane_ = 1;
  score_ = 0;
  spawnTimerMs_ = 0;
  speedPxPerSec_ = 18.0f;
  for (uint8_t i = 0; i < OBSTACLE_COUNT; i++) {
    obstacles_[i].active = false;
  }
}

void MicroRacerGame::updateRunning(uint32_t deltaMs, const ButtonInput& input) {
  const float deltaSec = static_cast<float>(deltaMs) * 0.001f;

  if (input.click) {
    playerLane_ = (playerLane_ + 1) % LANE_COUNT;
  }

  spawnTimerMs_ += deltaMs;
  if (spawnTimerMs_ >= 550) {
    spawnTimerMs_ = 0;
    spawnObstacle();
  }

  const float move = speedPxPerSec_ * deltaSec;
  for (uint8_t i = 0; i < OBSTACLE_COUNT; i++) {
    if (!obstacles_[i].active) {
      continue;
    }
    obstacles_[i].y += move;
    if (obstacles_[i].y > static_cast<float>(height)) {
      obstacles_[i].active = false;
      score_++;
      if ((score_ % 8) == 0 && speedPxPerSec_ < 38.0f) {
        speedPxPerSec_ += 2.0f;
      }
    }

    if (obstacles_[i].active &&
        obstacles_[i].lane == playerLane_ &&
        obstacles_[i].y >= static_cast<float>(PLAYER_Y - 3) &&
        obstacles_[i].y <= static_cast<float>(PLAYER_Y + 3)) {
      endGame();
    }
  }
}

void MicroRacerGame::drawRunning(U8G2& u8g2) {
  u8g2.drawFrame(left_, 0, width, height);
  u8g2.drawLine(left_ + 23, 1, left_ + 23, height - 2);
  u8g2.drawLine(left_ + 46, 1, left_ + 46, height - 2);

  for (uint8_t i = 0; i < OBSTACLE_COUNT; i++) {
    if (obstacles_[i].active) {
      u8g2.drawBox(left_ + laneX(obstacles_[i].lane), static_cast<int>(obstacles_[i].y), 8, 4);
    }
  }

  u8g2.drawFrame(left_ + laneX(playerLane_), PLAYER_Y, 8, 4);
  u8g2.setFont(u8g2_font_4x6_tr);
  u8g2.setCursor(left_ + 2, 6);
  u8g2.print(score_);
}

int MicroRacerGame::laneX(uint8_t lane) const {
  return 7 + lane * 23;
}

void MicroRacerGame::spawnObstacle() {
  for (uint8_t i = 0; i < OBSTACLE_COUNT; i++) {
    if (!obstacles_[i].active) {
      obstacles_[i].active = true;
      obstacles_[i].lane = random(0, LANE_COUNT);
      obstacles_[i].y = 0.0f;
      return;
    }
  }
}
