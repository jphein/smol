// ============================================================================
//  BLOCK DIGGER  —  a tiny Minecraft-ish game for ESP32-C3 + 0.42" OLED
//  ** Bluepad32 edition: play with a Bluetooth Stadia controller **
// ----------------------------------------------------------------------------
//  Board : ESP32-C3 SuperMini + 0.42" OLED (SSD1306, 72x40 visible, I2C)
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
//     D-pad L/R ... walk        D-pad UP ... jump one tile
//     A ......... dig           B ........ place a block
//   (Left analog stick also works for movement.)
//
//  GAMEPLAY: gravity pulls you down. Dig blocks into your inventory (counter
//  top-left), place them back to build. Dig down, build up. :)
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

// ---- World ---------------------------------------------------------------
static const int TILE = 6;
static const int MAXW = 128 / TILE + 1;
static const int MAXH = 64  / TILE + 1;

enum Block : uint8_t { AIR = 0, DIRT, STONE, GRASS };
uint8_t world[MAXH][MAXW];
int COLS, ROWS;

struct Player { int x, y, facing; } p;
int inventory = 0;
uint32_t lastFall = 0, lastMove = 0;
const uint32_t FALL_MS = 220;   // gravity tick
const uint32_t MOVE_MS = 140;   // walk auto-repeat while held

// ---- Bluepad32 controller ------------------------------------------------
ControllerPtr gamepad = nullptr;
uint16_t prevButtons = 0;       // for edge detection
uint8_t  prevDpad = 0;

// D-pad bit masks (Bluepad32 convention)
static const uint8_t DP_UP = 0x01, DP_DOWN = 0x02, DP_RIGHT = 0x04, DP_LEFT = 0x08;
static const int STICK_DEADZONE = 300;   // axis range is roughly -512..511

void onConnect(ControllerPtr ctl) {
  if (gamepad == nullptr) gamepad = ctl;
}
void onDisconnect(ControllerPtr ctl) {
  if (gamepad == ctl) gamepad = nullptr;
}

// ---- World helpers -------------------------------------------------------
bool solid(int c, int r) {
  if (c < 0 || c >= COLS || r < 0 || r >= ROWS) return true;
  return world[r][c] != AIR;
}

void generateWorld() {
  int surface = ROWS / 2;
  for (int r = 0; r < ROWS; r++)
    for (int c = 0; c < COLS; c++) {
      if (r < surface)          world[r][c] = AIR;
      else if (r == surface)    world[r][c] = GRASS;
      else if (r < surface + 2) world[r][c] = DIRT;
      else                      world[r][c] = STONE;
    }
  p = { COLS / 2, surface - 1, 1 };
  inventory = 0;
}

// ---- Game logic ----------------------------------------------------------
void tryMove(int dc) {
  p.facing = dc;
  int nx = p.x + dc;
  if (!solid(nx, p.y)) p.x = nx;
  else if (!solid(nx, p.y - 1) && !solid(p.x, p.y - 1)) { p.x = nx; p.y -= 1; }
}

void applyGravity() { if (!solid(p.x, p.y + 1)) p.y += 1; }

void dig() {
  int tc = p.x + p.facing, tr = p.y;
  if (!solid(tc, tr)) { tc = p.x; tr = p.y + 1; }
  if (tc >= 0 && tc < COLS && tr >= 0 && tr < ROWS && world[tr][tc] != AIR) {
    world[tr][tc] = AIR;
    if (inventory < 999) inventory++;
  }
}

void place() {
  if (inventory <= 0) return;
  int tc = p.x + p.facing, tr = p.y;
  if (solid(tc, tr)) { tc = p.x; tr = p.y + 1; }
  if (tc >= 0 && tc < COLS && tr >= 0 && tr < ROWS && world[tr][tc] == AIR
      && !(tc == p.x && tr == p.y)) {
    world[tr][tc] = DIRT;
    inventory--;
  }
}

// ---- Input ---------------------------------------------------------------
void handleInput() {
  BP32.update();
  if (!gamepad || !gamepad->isConnected() || !gamepad->isGamepad()) return;

  uint8_t dpad = gamepad->dpad();
  int ax = gamepad->axisX();          // left stick X
  int ay = gamepad->axisY();          // left stick Y (down = positive)

  // combine d-pad and analog stick into simple directional intents
  bool left  = (dpad & DP_LEFT)  || ax < -STICK_DEADZONE;
  bool right = (dpad & DP_RIGHT) || ax >  STICK_DEADZONE;
  bool up    = (dpad & DP_UP)    || ay < -STICK_DEADZONE;

  // walking auto-repeats while held
  if ((left || right) && millis() - lastMove > MOVE_MS) {
    tryMove(left ? -1 : +1);
    lastMove = millis();
  }

  // JUMP on the rising edge of up (d-pad or stick)
  static bool prevUp = false;
  if (up && !prevUp && !solid(p.x, p.y - 1)) p.y -= 1;
  prevUp = up;

  // A = dig, B = place — edge triggered so one press = one action
  bool aNow = gamepad->a(), bNow = gamepad->b();
  static bool prevA = false, prevB = false;
  if (aNow && !prevA) dig();
  if (bNow && !prevB) place();
  prevA = aNow; prevB = bNow;
}

// ---- Rendering -----------------------------------------------------------
void drawBlock(int c, int r, uint8_t type) {
  int x = c * TILE, y = r * TILE;
  switch (type) {
    case GRASS: u8g2.drawBox(x, y, TILE, TILE);
                u8g2.setDrawColor(0); u8g2.drawPixel(x+1,y+1); u8g2.drawPixel(x+3,y+2);
                u8g2.setDrawColor(1); break;
    case DIRT:  u8g2.drawFrame(x, y, TILE, TILE); u8g2.drawPixel(x+2,y+2); break;
    case STONE: u8g2.drawBox(x, y, TILE, TILE); break;
    default: break;
  }
}

void render() {
  u8g2.clearBuffer();
  for (int r = 0; r < ROWS; r++)
    for (int c = 0; c < COLS; c++)
      if (world[r][c] != AIR) drawBlock(c, r, world[r][c]);

  int px = p.x * TILE, py = p.y * TILE;
  u8g2.drawBox(px, py, TILE, TILE);
  u8g2.setDrawColor(0);
  u8g2.drawPixel(px + (p.facing > 0 ? TILE-2 : 1), py + 1);
  u8g2.setDrawColor(1);

  u8g2.setFont(u8g2_font_4x6_tf);
  u8g2.setCursor(0, 6);
  if (!gamepad) u8g2.print("pair..");   // waiting for controller
  else          u8g2.print(inventory);

  u8g2.sendBuffer();
}

// ---- Arduino entry points ------------------------------------------------
void setup() {
  Wire.begin(I2C_SDA, I2C_SCL);
  u8g2.begin();
  u8g2.setBusClock(400000);

  COLS = min((int)(u8g2.getDisplayWidth()  / TILE), MAXW);
  ROWS = min((int)(u8g2.getDisplayHeight() / TILE), MAXH);
  generateWorld();

  BP32.setup(&onConnect, &onDisconnect);
  BP32.forgetBluetoothKeys();     // start clean each boot (remove once paired reliably)
  BP32.enableVirtualDevice(false);
}

void loop() {
  handleInput();
  if (millis() - lastFall > FALL_MS) { applyGravity(); lastFall = millis(); }
  render();
  delay(16);                      // ~60 fps cap
}
