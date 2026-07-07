#pragma once

#include "Game.h"

class HeliCaveGame : public Game {
  public:
    HeliCaveGame(uint32_t width, uint32_t height, uint32_t left);

  protected:
    void onGameReset() override;
    void updateRunning(uint32_t deltaMs, const ButtonInput& input) override;
    void drawRunning(U8G2& u8g2) override;

  private:
    static constexpr uint8_t PLAYER_W = 5;
    static constexpr uint8_t PLAYER_H = 3;
    static constexpr uint8_t SEGMENT_COUNT = 14;
    static constexpr uint8_t SEGMENT_W = 6;
    static constexpr uint8_t GAP_HEIGHT = 22;
    static constexpr uint8_t MAX_GAP_STEP = 1;

    struct Segment {
      float x = 0.0f;
      int gapTop = 0;
      int gapHeight = 0;
      bool active = false;
    };

    uint16_t nextRand();
    int clampInt(int value, int minValue, int maxValue) const;
    int nextGapTop(int previousGapTop);
    void spawnSegment(float x);
    bool collidesWithSegment(const Segment& segment) const;

    uint32_t left_;
    float playerY_ = 0.0f;
    float playerVy_ = 0.0f;
    float scrollSpeed_ = 16.0f;
    int lastGapTop_ = 10;
    uint16_t randState_ = 0xACE1;
    Segment segments_[SEGMENT_COUNT] = {};
};
