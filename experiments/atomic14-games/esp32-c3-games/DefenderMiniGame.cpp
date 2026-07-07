#include "DefenderMiniGame.h"

#include <Arduino.h>
#include <U8g2lib.h>

DefenderMiniGame::DefenderMiniGame(uint32_t width, uint32_t height, uint32_t left)
    : Game("Defender Mini", width, height), left_(left) {}

void DefenderMiniGame::onGameReset() {
  shipBand_ = 1;
  score_ = 0;
  spawnTimerMs_ = 0;
  shotTimerMs_ = 0;
  for (uint8_t i = 0; i < ENEMY_COUNT; i++) {
    enemies_[i].active = false;
  }
  for (uint8_t i = 0; i < SHOT_COUNT; i++) {
    shots_[i].active = false;
  }
}

void DefenderMiniGame::updateRunning(uint32_t deltaMs, const ButtonInput& input) {
  const float deltaSec = static_cast<float>(deltaMs) * 0.001f;

  if (input.click) {
    shipBand_ = (shipBand_ + 1) % BAND_COUNT;
  }

  spawnTimerMs_ += deltaMs;
  if (spawnTimerMs_ >= 700) {
    spawnTimerMs_ = 0;
    spawnEnemy();
  }

  shotTimerMs_ += deltaMs;
  if (shotTimerMs_ >= 350) {
    shotTimerMs_ = 0;
    spawnShot();
  }

  const float enemyMove = 18.0f * deltaSec;
  const float shotMove = 40.0f * deltaSec;

  for (uint8_t i = 0; i < ENEMY_COUNT; i++) {
    if (!enemies_[i].active) {
      continue;
    }
    enemies_[i].x -= enemyMove;
    if (enemies_[i].x <= 5.0f && enemies_[i].band == shipBand_) {
      endGame();
    } else if (enemies_[i].x < 0.0f) {
      enemies_[i].active = false;
    }
  }

  for (uint8_t i = 0; i < SHOT_COUNT; i++) {
    if (!shots_[i].active) {
      continue;
    }
    shots_[i].x += shotMove;
    if (shots_[i].x > static_cast<float>(width)) {
      shots_[i].active = false;
    }
  }

  for (uint8_t e = 0; e < ENEMY_COUNT; e++) {
    if (!enemies_[e].active) {
      continue;
    }
    for (uint8_t s = 0; s < SHOT_COUNT; s++) {
      if (!shots_[s].active) {
        continue;
      }
      if (shots_[s].band == enemies_[e].band &&
          shots_[s].x >= enemies_[e].x &&
          shots_[s].x <= enemies_[e].x + 3.0f) {
        shots_[s].active = false;
        enemies_[e].active = false;
        score_++;
        break;
      }
    }
  }
}

void DefenderMiniGame::drawRunning(U8G2& u8g2) {
  u8g2.drawFrame(left_, 0, width, height);
  u8g2.drawBox(left_ + 3, bandY(shipBand_), 4, 3);
  for (uint8_t i = 0; i < ENEMY_COUNT; i++) {
    if (enemies_[i].active) {
      u8g2.drawFrame(left_ + static_cast<int>(enemies_[i].x), bandY(enemies_[i].band), 4, 3);
    }
  }
  for (uint8_t i = 0; i < SHOT_COUNT; i++) {
    if (shots_[i].active) {
      u8g2.drawPixel(left_ + static_cast<int>(shots_[i].x), bandY(shots_[i].band) + 1);
    }
  }
  u8g2.setFont(u8g2_font_4x6_tr);
  u8g2.setCursor(left_ + 2, 6);
  u8g2.print(score_);
}

int DefenderMiniGame::bandY(uint8_t band) const {
  return 8 + band * 10;
}

void DefenderMiniGame::spawnEnemy() {
  for (uint8_t i = 0; i < ENEMY_COUNT; i++) {
    if (!enemies_[i].active) {
      enemies_[i].active = true;
      enemies_[i].x = static_cast<float>(width - 5);
      enemies_[i].band = random(0, BAND_COUNT);
      return;
    }
  }
}

void DefenderMiniGame::spawnShot() {
  for (uint8_t i = 0; i < SHOT_COUNT; i++) {
    if (!shots_[i].active) {
      shots_[i].active = true;
      shots_[i].x = 8.0f;
      shots_[i].band = shipBand_;
      return;
    }
  }
}
