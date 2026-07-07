#pragma once

#include "Game.h"

class DefenderMiniGame : public Game {
  public:
    DefenderMiniGame(uint32_t width, uint32_t height, uint32_t left);

  protected:
    void onGameReset() override;
    void updateRunning(uint32_t deltaMs, const ButtonInput& input) override;
    void drawRunning(U8G2& u8g2) override;

  private:
    static constexpr uint8_t BAND_COUNT = 3;
    static constexpr uint8_t ENEMY_COUNT = 6;
    static constexpr uint8_t SHOT_COUNT = 6;

    struct Enemy {
      float x = 0.0f;
      uint8_t band = 0;
      bool active = false;
    };

    struct Shot {
      float x = 0.0f;
      uint8_t band = 0;
      bool active = false;
    };

    int bandY(uint8_t band) const;
    void spawnEnemy();
    void spawnShot();

    uint32_t left_;
    uint8_t shipBand_ = 1;
    uint16_t spawnTimerMs_ = 0;
    uint16_t shotTimerMs_ = 0;
    int score_ = 0;
    Enemy enemies_[ENEMY_COUNT] = {};
    Shot shots_[SHOT_COUNT] = {};
};
