// ============================================================================
//  SNAKE  —  the classic, for ESP32-C3 + 0.42" OLED
//  ** Bluepad32 edition: play with a Bluetooth Stadia controller **
// ----------------------------------------------------------------------------
//  Board : ESP32-C3 SuperMini + 0.42" OLED (SSD1306, 72x40 visible, I2C)
//
//  BUILD NOTE:
//    FQBN: esp32-bluepad32:esp32:esp32c3:CDCOnBoot=cdc,FlashSize=4M
//    Requires libraries: U8g2 (by olikraus) + Bluepad32.
//
//  LIBRARIES / BOARD SETUP (do this once):
//   1. In Arduino IDE > Preferences > "Additional Boards Manager URLs" add:
//        https://raw.githubusercontent.com/ricardoquesada/esp32-arduino-lib-builder/master/bluepad32_files/package_esp32_bluepad32_index.json
//   2. Boards Manager > install "esp32_bluepad32".
//   3. Select board:  "ESP32C3 Dev Module (Bluepad32)".
//      (This bundles the BLE/BTstack that Bluepad32 needs.)
//   4. Library Manager > install "U8g2" (by olikraus).
//
//  CONTROLLER (Stadia, flashed to Bluetooth mode):
//   Power on the controller in pairing mode; the ESP32 auto-connects on boot.
//     D-pad ..... steer the snake (can't reverse straight into yourself)
//     A ......... restart after game over
//   (Left analog stick also steers.)
//
//  GAMEPLAY: eat the food (a small square), grow longer, rack up points
//  (counter top-left). Hit a wall or your own tail and it's game over —
//  press A to start a fresh run.
// ============================================================================

#include <Bluepad32.h>
#include <U8g2lib.h>
#include <Wire.h>

// ---- Display -------------------------------------------------------------
U8G2_SSD1306_72X40_ER_F_HW_I2C u8g2(U8G2_R0, /*reset=*/U8X8_PIN_NONE);
// Alternatives for other screens (swap the line above):
// U8G2_SSD1306_128X64_NONAME_F_HW_I2C u8g2(U8G2_R0, U8X8_PIN_NONE);
static const int I2C_SDA = 5;
static const int I2C_SCL = 6;

// ---- Grid ----------------------------------------------------------------
// TILE=4 gives an 18x10 grid on the 72x40 panel. COLS/ROWS are derived from
// the actual display size at runtime (see setup), so a bigger panel just
// yields a bigger board. MAXW/MAXH size the fixed buffers for the worst case.
static const int TILE = 4;
static const int MAXW = 128 / TILE + 1;
static const int MAXH = 64  / TILE + 1;
static const int MAXCELLS = MAXW * MAXH;   // upper bound on snake length

int COLS, ROWS;

// ---- Snake state ---------------------------------------------------------
// The snake body is a ring buffer of grid cells. body[head] is the head;
// the tail sits `length` cells behind it. We store x/y in parallel arrays.
uint8_t bodyX[MAXCELLS];
uint8_t bodyY[MAXCELLS];
int head;                 // index of the head cell in the ring buffer
int length;               // number of occupied cells (>= 1)

int dirX, dirY;           // current heading (one of them is 0, the other +/-1)
int pendingX, pendingY;   // heading queued from input, applied on the next tick

int foodX, foodY;         // food cell
int score;
bool gameOver;

uint32_t lastStep = 0;    // last movement tick (millis)
const uint32_t STEP_MS = 140;   // snake advances one cell every STEP_MS

// ---- Bluepad32 controller ------------------------------------------------
ControllerPtr gamepad = nullptr;

// D-pad bit masks (Bluepad32 convention)
static const uint8_t DP_UP = 0x01, DP_DOWN = 0x02, DP_RIGHT = 0x04, DP_LEFT = 0x08;
static const int STICK_DEADZONE = 300;   // axis range is roughly -512..511

void onConnect(ControllerPtr ctl) {
  if (gamepad == nullptr) gamepad = ctl;
}
void onDisconnect(ControllerPtr ctl) {
  if (gamepad == ctl) gamepad = nullptr;
}

// ---- Snake helpers -------------------------------------------------------
// Ring-buffer index math: walking backwards from the head by `offset` cells.
int cellIndex(int offset) {
  return (head - offset + MAXCELLS) % MAXCELLS;
}

// Is grid cell (x,y) currently occupied by the snake body? `ignoreTail`
// skips the very last cell, which vacates on the same tick the head advances.
bool onSnake(int x, int y, bool ignoreTail) {
  int cells = length - (ignoreTail ? 1 : 0);
  for (int i = 0; i < cells; i++) {
    int idx = cellIndex(i);
    if (bodyX[idx] == x && bodyY[idx] == y) return true;
  }
  return false;
}

// Drop food on a random empty cell (never under the snake).
void spawnFood() {
  // Bail out gracefully if the board is somehow full (win condition).
  if (length >= COLS * ROWS) { foodX = -1; foodY = -1; return; }
  int x, y;
  do {
    x = random(COLS);
    y = random(ROWS);
  } while (onSnake(x, y, false));
  foodX = x;
  foodY = y;
}

// (Re)start a fresh game: short snake in the middle, heading right.
void resetGame() {
  COLS = min((int)(u8g2.getDisplayWidth()  / TILE), MAXW);
  ROWS = min((int)(u8g2.getDisplayHeight() / TILE), MAXH);

  length = 3;
  head = length - 1;                // head is the last cell we lay down
  int cx = COLS / 2, cy = ROWS / 2;
  for (int i = 0; i < length; i++) {
    // Lay the body left-to-right so the head ends up on the right, tail left.
    bodyX[i] = cx - (length - 1 - i);
    bodyY[i] = cy;
  }

  dirX = 1; dirY = 0;               // moving right
  pendingX = dirX; pendingY = dirY;

  score = 0;
  gameOver = false;

  randomSeed(micros());
  spawnFood();
}

// Advance the snake by one cell; handles food, growth, and collisions.
void step() {
  // Commit the queued direction now (input can't reverse it — see handleInput).
  dirX = pendingX; dirY = pendingY;

  int nx = bodyX[head] + dirX;
  int ny = bodyY[head] + dirY;

  // Wall collision = game over.
  if (nx < 0 || nx >= COLS || ny < 0 || ny >= ROWS) { gameOver = true; return; }

  // Self collision. The tail cell is about to move, so ignore it — following
  // your own tail is legal (unless we're also about to grow onto it).
  bool eating = (nx == foodX && ny == foodY);
  if (onSnake(nx, ny, /*ignoreTail=*/!eating)) { gameOver = true; return; }

  // Advance the head into the new cell.
  head = (head + 1) % MAXCELLS;
  bodyX[head] = nx;
  bodyY[head] = ny;

  if (eating) {
    // Grow: keep the tail this tick, then drop new food.
    if (length < MAXCELLS) length++;
    score++;
    spawnFood();
  }
  // If not eating, `length` is unchanged so the tail cell is simply dropped
  // from the drawn/collision set — no array shuffling needed.
}

// ---- Input ---------------------------------------------------------------
void handleInput() {
  BP32.update();
  if (!gamepad || !gamepad->isConnected() || !gamepad->isGamepad()) return;

  // On game over, A starts a new run (edge-triggered so one press = one restart).
  if (gameOver) {
    bool aNow = gamepad->a();
    static bool prevA = false;
    if (aNow && !prevA) resetGame();
    prevA = aNow;
    return;
  }

  uint8_t dpad = gamepad->dpad();
  int ax = gamepad->axisX();          // left stick X
  int ay = gamepad->axisY();          // left stick Y (down = positive)

  // Combine d-pad and analog stick into directional intents.
  bool left  = (dpad & DP_LEFT)  || ax < -STICK_DEADZONE;
  bool right = (dpad & DP_RIGHT) || ax >  STICK_DEADZONE;
  bool up    = (dpad & DP_UP)    || ay < -STICK_DEADZONE;
  bool down  = (dpad & DP_DOWN)  || ay >  STICK_DEADZONE;

  // Queue a turn. Reject any 180° reversal (relative to the *committed*
  // heading) so you can't fold straight back into your own neck.
  if      (left  && dirX == 0) { pendingX = -1; pendingY = 0; }
  else if (right && dirX == 0) { pendingX =  1; pendingY = 0; }
  else if (up    && dirY == 0) { pendingX = 0;  pendingY = -1; }
  else if (down  && dirY == 0) { pendingX = 0;  pendingY =  1; }
}

// ---- Rendering -----------------------------------------------------------
void render() {
  u8g2.clearBuffer();

  // Snake body: filled tiles with a 1px gap so segments read distinctly.
  for (int i = 0; i < length; i++) {
    int idx = cellIndex(i);
    u8g2.drawBox(bodyX[idx] * TILE, bodyY[idx] * TILE, TILE - 1, TILE - 1);
  }

  // Food: a small centered square, drawn only while a valid cell exists.
  if (foodX >= 0) {
    u8g2.drawBox(foodX * TILE + 1, foodY * TILE + 1, TILE - 2, TILE - 2);
  }

  // HUD (top-left), matching blockdigger's tiny font + cursor placement.
  u8g2.setFont(u8g2_font_4x6_tf);
  u8g2.setCursor(0, 6);
  if (!gamepad)       u8g2.print("pair..");   // waiting for controller
  else if (gameOver)  { u8g2.print("DEAD "); u8g2.print(score); }
  else                u8g2.print(score);

  u8g2.sendBuffer();
}

// ---- Arduino entry points ------------------------------------------------
void setup() {
  Wire.begin(I2C_SDA, I2C_SCL);
  u8g2.begin();
  u8g2.setBusClock(400000);

  resetGame();                    // sizes the board + lays out the first snake

  BP32.setup(&onConnect, &onDisconnect);
  BP32.forgetBluetoothKeys();     // start clean each boot (remove once paired reliably)
  BP32.enableVirtualDevice(false);
}

void loop() {
  handleInput();

  // Steady movement tick: the snake advances one cell every STEP_MS,
  // independent of the (faster) render loop.
  if (!gameOver && millis() - lastStep > STEP_MS) {
    step();
    lastStep = millis();
  }

  render();
  delay(16);                      // ~60 fps cap
}
