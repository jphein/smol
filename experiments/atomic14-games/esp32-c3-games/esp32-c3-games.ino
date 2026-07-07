#include <U8g2lib.h>
#include <Wire.h>

#include "BreakoutGame.h"
#include "DefenderMiniGame.h"
#include "Game.h"
#include "HeliCaveGame.h"
#include "JumpGame.h"
#include "MicroRacerGame.h"

class U8G2_SSD1306_72X40_NONAME_F_HW_I2C : public U8G2 {
  public:
    U8G2_SSD1306_72X40_NONAME_F_HW_I2C(
        const u8g2_cb_t* rotation, uint8_t reset = U8X8_PIN_NONE, uint8_t clock = U8X8_PIN_NONE, uint8_t data = U8X8_PIN_NONE)
        : U8G2() {
      u8g2_Setup_ssd1306_i2c_72x40_er_f(&u8g2, rotation, u8x8_byte_arduino_hw_i2c, u8x8_gpio_and_delay_arduino);
      u8x8_SetPin_HW_I2C(getU8x8(), reset, clock, data);
    }
};

U8G2_SSD1306_72X40_NONAME_F_HW_I2C u8g2(U8G2_R0, U8X8_PIN_NONE, 6, 5);

constexpr uint8_t BUTTON_PIN = 9;
constexpr uint8_t SCREEN_WIDTH = 72;
constexpr uint8_t SCREEN_HEIGHT = 40;
constexpr uint8_t GAME_WIDTH = 70;
constexpr uint8_t GAME_HEIGHT = 40;
constexpr uint8_t GAME_LEFT = 1;

bool isButtonDown() {
  return digitalRead(BUTTON_PIN) == LOW;
}

BreakoutGame breakoutGame(GAME_WIDTH, GAME_HEIGHT, GAME_LEFT);
MicroRacerGame microRacerGame(GAME_WIDTH, GAME_HEIGHT, GAME_LEFT);
DefenderMiniGame defenderMiniGame(GAME_WIDTH, GAME_HEIGHT, GAME_LEFT);
JumpGame jumpGame(GAME_WIDTH, GAME_HEIGHT, GAME_LEFT);
HeliCaveGame heliCaveGame(GAME_WIDTH, GAME_HEIGHT, GAME_LEFT);

Game* games[] = {&breakoutGame, &microRacerGame, &defenderMiniGame, &jumpGame, &heliCaveGame};
constexpr uint8_t GAME_COUNT = sizeof(games) / sizeof(games[0]);

SingleButton menuButton;
uint8_t menuIndex = 0;
bool inMenu = true;
bool menuLaunchArmed = false;
Game* activeGame = nullptr;

void drawMenu() {
  u8g2.drawFrame(0, 0, SCREEN_WIDTH, SCREEN_HEIGHT);
  u8g2.setFont(u8g2_font_5x8_tr);
  u8g2.drawStr(3, 9, "Select game");
  u8g2.drawStr(3, 19, games[menuIndex]->gameTitle());
  if (menuLaunchArmed) {
    u8g2.drawStr(3, 29, "Release");
    u8g2.drawStr(3, 38, "to start");
  } else {
    u8g2.drawStr(3, 29, "Tap next");
    u8g2.drawStr(3, 38, "Hold start");
  }
}

void drawGameOverlay(Game& game) {
  u8g2.drawFrame(0, 0, SCREEN_WIDTH, SCREEN_HEIGHT);
  u8g2.setFont(u8g2_font_5x8_tr);
  if (game.phase() == GamePhase::Start) {
    u8g2.drawStr(3, 10, game.gameTitle());
    u8g2.drawStr(3, 24, "Click to start");
  } else if (game.phase() == GamePhase::End) {
    u8g2.drawStr(3, 10, "Game Over");
    u8g2.drawStr(3, 24, "Click restart");
    u8g2.drawStr(3, 36, "Hold menu");
  }
}

void setup(void) {
  pinMode(BUTTON_PIN, INPUT_PULLUP);
  u8g2.begin();
  u8g2.setContrast(255);
  u8g2.setBusClock(400000);
  randomSeed(micros());
  menuButton.reset(isButtonDown(), millis());
  menuLaunchArmed = false;
}

void loop(void) {
  const uint32_t nowMs = millis();
  const bool buttonDown = isButtonDown();

  u8g2.clearBuffer();

  if (inMenu) {
    const ButtonInput input = menuButton.update(buttonDown, nowMs);
    if (input.longPress) {
      menuLaunchArmed = true;
    }
    if (input.click && !menuLaunchArmed) {
      menuIndex = (menuIndex + 1) % GAME_COUNT;
    }
    if (menuLaunchArmed && input.released) {
      activeGame = games[menuIndex];
      activeGame->begin(nowMs, buttonDown);
      inMenu = false;
      menuLaunchArmed = false;
    }
    drawMenu();
  } else {
    activeGame->tick(nowMs, buttonDown);
    activeGame->render(u8g2);
    if (activeGame->phase() != GamePhase::Running) {
      drawGameOverlay(*activeGame);
    }
    if (activeGame->shouldExitToMenu()) {
      activeGame->clearExitRequest();
      menuButton.reset(buttonDown, nowMs);
      inMenu = true;
      menuLaunchArmed = false;
    }
  }

  u8g2.sendBuffer();
}
