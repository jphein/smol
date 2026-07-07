// ============================================================================
//  SNAKE — 2-PLAYER, NETWORKED over ESP-NOW
//  Two ESP32-C3 + 0.42" OLED boards, each running one snake, synced wirelessly.
// ----------------------------------------------------------------------------
//  Board : ESP32-C3 SuperMini + 0.42" OLED (SSD1306, 72x40 visible, I2C)
//
//  BUILD NOTE:
//    FQBN: esp32:esp32:esp32c3:CDCOnBoot=cdc,FlashSize=4M
//      (plain espressif "esp32" core — NOT the bluepad32 fork — because this
//       build uses WiFi/ESP-NOW and no BLE. See the INPUT note below.)
//    Requires library: U8g2 (by olikraus).
//    ESP-NOW + WiFi come with the espressif esp32 Arduino core; no extra libs.
//
//  LIBRARIES / BOARD SETUP (once):
//   1. Arduino IDE > Boards Manager: install "esp32" by Espressif Systems.
//   2. Select board: "ESP32C3 Dev Module" (plain, not the Bluepad32 variant).
//   3. Tools: USB CDC On Boot = "Enabled",  Flash Size = "4MB".
//   4. Library Manager > install "U8g2" (by olikraus).
//
//  FLASH BOTH BOARDS with this identical sketch. They auto-negotiate who is
//  "host" (the board with the numerically lower Wi-Fi MAC), so there is nothing
//  per-board to change. Just power up two boards near each other.
//
// ----------------------------------------------------------------------------
//  INPUT — WHY THE BOOT BUTTON, NOT BLUETOOTH  (honest caveat)
// ----------------------------------------------------------------------------
//  The single-player snake.ino steers with a Bluepad32 (BLE) gamepad. On the
//  single-core ESP32-C3, running BLE (Bluepad32 / BTstack) AND Wi-Fi (ESP-NOW)
//  *at the same time* is heavy and finicky: the two radios time-share one 2.4GHz
//  front-end, RAM is tight, and the Bluepad32 board package ships a different
//  radio stack than the plain esp32 core that ESP-NOW wants. It can be made to
//  work, but it is fragile and hard to demo reliably.
//
//  So for a ROBUST, compile-and-go 2-player demo, each player steers with the
//  board's onboard BOOT button (GPIO9):
//        TAP  = turn RIGHT (rotate heading 90 clockwise)
//  One button can reach every direction by tapping (right, right, right = left).
//  This is exactly the input model of atomic14's single-button C3 games that
//  were built for this board, so it is idiomatic here. With no BLE running,
//  ESP-NOW gets the radio to itself and the link is stable.
//
//  (If you MUST use a gamepad: keep Bluepad32 for input, drop back to the
//  bluepad32 board package, and expect the coexistence quirks above. That path
//  is intentionally NOT taken here.)
//
// ----------------------------------------------------------------------------
//  GAMEPLAY
//   - Both boards render BOTH snakes: YOUR snake is drawn filled, the OPPONENT
//     is drawn as hollow (outlined) tiles, so you can tell them apart.
//   - Food is shared. The host board decides where food spawns (with an RNG the
//     client never runs) and broadcasts it; both boards eat from the same food.
//   - Eating food grows your snake and scores a point.
//   - Collision ends the ROUND for everyone: hitting a wall, your own tail, or
//     the OTHER snake's body. The round result (who died) is shown on both.
//   - After game over, TAP the button to vote "ready"; when both are ready
//     (or after a short host-driven grace) a new round starts, synced.
// ============================================================================

#include <esp_now.h>
#include <WiFi.h>
#include <esp_wifi.h>      // esp_wifi_set_channel / set_promiscuous (fixed-channel pin)
#include <U8g2lib.h>
#include <Wire.h>

// ---- Forward declarations (functions used before they are defined) --------
// The Arduino IDE auto-prototypes most functions, but ones taking pointer args
// and called earlier in the file are safest declared explicitly.
int  macCompare(const uint8_t *a, const uint8_t *b);
void decideRoleAgainstPeer(const uint8_t *pm);
void resetRound(uint8_t gid);
void sendState();

// ---- Display (same constructor/pins as single-player snake.ino) ----------
U8G2_SSD1306_72X40_ER_F_HW_I2C u8g2(U8G2_R0, /*reset=*/U8X8_PIN_NONE);
static const int I2C_SDA = 5;
static const int I2C_SCL = 6;

// ---- Input: onboard BOOT button ------------------------------------------
static const int BTN_PIN = 9;          // GPIO9 = BOOT button (active LOW)
static const uint32_t DEBOUNCE_MS = 30;

// ---- Grid (same sizing scheme as single-player) --------------------------
// TILE=4 => an 18x10 grid on the 72x40 panel. COLS/ROWS are derived from the
// real display size at runtime. MAXW/MAXH bound the fixed buffers.
static const int TILE = 4;
static const int MAXW = 128 / TILE + 1;
static const int MAXH = 64  / TILE + 1;
static const int MAXCELLS = MAXW * MAXH;   // upper bound on one snake's length

int COLS, ROWS;

// ---- Movement tick --------------------------------------------------------
static const uint32_t STEP_MS = 160;   // both snakes advance one cell per tick
uint32_t lastStep = 0;

// ---- ESP-NOW link ---------------------------------------------------------
// Broadcast to everyone on a fixed channel; the peer(s) filter by our protocol.
static const uint8_t WIFI_CHANNEL = 1;                 // MUST match on both boards
uint8_t BROADCAST_ADDR[6] = {0xFF,0xFF,0xFF,0xFF,0xFF,0xFF};

static const uint8_t PROTO_MAGIC = 0x53;   // 'S' — sanity tag for our packets
static const uint8_t PROTO_VER   = 1;

// A single packet carries a snake's full state for the current round.
//
// Body packing: to fit the WHOLE board under ESP-NOW's 250-byte payload limit
// we store each occupied cell as ONE byte — a linear index cell = x*ROWS + y.
// On the 72x40 panel the grid is 18x10 = 180 cells, so the largest index is
// 179, which fits in a uint8_t. That lets a maximally-long snake (up to the
// full board) travel intact. (On a bigger 128x64 dev panel the grid is 33x17 =
// 561 cells > 255, so a linear byte index would alias — see BODY_CAP note; the
// 72x40 target board is fully covered.)
//
//   sizeof(SnakePacket) with BODY_CAP=200:  ~19 header + 200 body = ~219 bytes.
static const int BODY_CAP = 200;   // >= 180 (full 72x40 board); 200B body keeps us < 250B

typedef struct __attribute__((packed)) {
  uint8_t  magic;        // PROTO_MAGIC
  uint8_t  ver;          // PROTO_VER
  uint8_t  playerId;     // 0 = host, 1 = client (decided by MAC compare)
  uint8_t  gameId;       // round id; bumped each new round so stale packets drop
  uint8_t  cols, rows;   // sender's board dims (receiver uses rows to unpack cells)

  int8_t   dirX, dirY;   // sender's heading
  uint16_t score;
  uint8_t  alive;        // 1 = alive this round, 0 = dead (collided)
  uint8_t  ready;        // 1 = pressed "ready" at game over

  // Host-authoritative shared food. Clients copy these; host owns them.
  int8_t   foodX, foodY; // -1,-1 = none
  uint8_t  foodSeq;      // bumped every time host respawns food

  uint16_t len;               // number of valid body cells (<= BODY_CAP)
  uint8_t  cells[BODY_CAP];   // cells[0]=tail .. cells[len-1]=head; each = x*rows + y
} SnakePacket;   // ~219 bytes packed  (< 250B ESP-NOW limit)

// Fail the build (rather than silently truncate at runtime) if the packet ever
// grows past what a single ESP-NOW frame can carry.
static_assert(sizeof(SnakePacket) <= 250, "SnakePacket exceeds ESP-NOW 250-byte payload limit");

// ---- Roles & identity -----------------------------------------------------
uint8_t myMac[6];
uint8_t myPlayerId = 0;          // 0=host, 1=client; set in setup after we learn peer
bool    amHost = true;           // host owns food RNG + round arbitration
bool    peerKnown = false;       // have we ever heard from the other board?
uint8_t peerMac[6];
uint32_t lastPeerMsg = 0;        // millis of last received packet
static const uint32_t PEER_TIMEOUT_MS = 1500;   // link considered "lost" after this

// ---- Per-snake state ------------------------------------------------------
// We keep two snakes: index 0 = host's snake, index 1 = client's snake.
// mySlot is whichever one THIS board simulates (== myPlayerId). The other slot
// is a shadow we fill from received packets and only render.
struct Snake {
  uint8_t bodyX[MAXCELLS];
  uint8_t bodyY[MAXCELLS];
  int     head;          // ring-buffer index of the head
  int     length;
  int     dirX, dirY;
  int     pendingX, pendingY;
  int     score;
  bool    alive;         // false once it has collided this round
  bool    ready;         // pressed ready at game over
  bool    valid;         // have we received/initialized this snake yet?
};

Snake snakes[2];
int mySlot = 0;          // == myPlayerId
int oppSlot = 1;

// ---- Shared food (host-authoritative) ------------------------------------
int foodX = -1, foodY = -1;
uint8_t foodSeq = 0;

// ---- Round / game control -------------------------------------------------
uint8_t gameId = 0;      // current round id
bool    roundOver = false;
int8_t  loserId  = -1;   // who died first (-1 none, 2 = both/draw)

// ---- Send throttling ------------------------------------------------------
uint32_t lastSend = 0;
static const uint32_t SEND_MIN_MS = 40;   // don't flood; we also send on each tick

// ============================================================================
//  Ring-buffer helpers (mirror the single-player logic)
// ============================================================================
int cellIndex(const Snake &s, int offset) {
  return (s.head - offset + MAXCELLS) % MAXCELLS;
}

// Is cell (x,y) on snake s? ignoreTail skips the last cell (it vacates as the
// head advances this same tick).
bool onSnakeBody(const Snake &s, int x, int y, bool ignoreTail) {
  if (!s.valid) return false;
  int cells = s.length - (ignoreTail ? 1 : 0);
  for (int i = 0; i < cells; i++) {
    int idx = cellIndex(s, i);
    if (s.bodyX[idx] == x && s.bodyY[idx] == y) return true;
  }
  return false;
}

// ============================================================================
//  Food (only the host actually decides; client copies via packet)
// ============================================================================
void hostSpawnFood() {
  // Never place under either snake. If board is packed, no food (edge case).
  int total = (snakes[0].valid ? snakes[0].length : 0) +
              (snakes[1].valid ? snakes[1].length : 0);
  if (total >= COLS * ROWS) { foodX = -1; foodY = -1; foodSeq++; return; }
  int x, y, guard = 0;
  do {
    x = random(COLS);
    y = random(ROWS);
    if (++guard > 500) break;   // pathological safety
  } while (onSnakeBody(snakes[0], x, y, false) ||
           onSnakeBody(snakes[1], x, y, false));
  foodX = x; foodY = y;
  foodSeq++;
}

// ============================================================================
//  Round setup — deterministic so both boards lay out identical start snakes
// ============================================================================
void initSnake(Snake &s, int startCol, int startRow, int dx, int dy) {
  s.length = 3;
  s.head = s.length - 1;
  for (int i = 0; i < s.length; i++) {
    // Lay body out behind the head along -(dx,dy) so head faces (dx,dy).
    s.bodyX[i] = startCol - (s.length - 1 - i) * dx;
    s.bodyY[i] = startRow - (s.length - 1 - i) * dy;
  }
  s.dirX = dx; s.dirY = dy;
  s.pendingX = dx; s.pendingY = dy;
  s.score = 0;
  s.alive = true;
  s.ready = false;
  s.valid = true;
}

// Start a fresh round. gid is the round id (host picks a new one; client adopts
// the host's). Both boards place both snakes identically from gid so the shadow
// snake looks right until the first packet arrives.
void resetRound(uint8_t gid) {
  COLS = min((int)(u8g2.getDisplayWidth()  / TILE), MAXW);
  ROWS = min((int)(u8g2.getDisplayHeight() / TILE), MAXH);

  gameId = gid;
  roundOver = false;
  loserId = -1;

  // Host snake (id 0): left side, heading right.
  initSnake(snakes[0], 2, ROWS / 2, 1, 0);
  // Client snake (id 1): right side, heading left.
  initSnake(snakes[1], COLS - 3, ROWS / 2, -1, 0);

  if (amHost) {
    randomSeed(micros() ^ (gid * 2654435761UL));
    hostSpawnFood();
  } else {
    // Client waits for host's food packet; show nothing until then.
    foodX = -1; foodY = -1;
  }
}

// ============================================================================
//  Input: BOOT button, tap = turn right (clockwise). Debounced + edge-detected.
// ============================================================================
// Rotating heading 90 clockwise on a screen where +y is DOWN:
//   right(1,0) -> down(0,1) -> left(-1,0) -> up(0,-1) -> right...
void rotateRight(int &dx, int &dy) {
  int ndx = -dy;
  int ndy =  dx;
  dx = ndx; dy = ndy;
}

// Returns true exactly once per physical press (falling edge, debounced).
bool buttonTapped() {
  static int      stableState = HIGH;   // pull-up: released = HIGH
  static int      lastReading = HIGH;
  static uint32_t lastChange  = 0;
  static bool     pressLatched = false;

  int reading = digitalRead(BTN_PIN);
  if (reading != lastReading) {
    lastReading = reading;
    lastChange = millis();
  }
  if (millis() - lastChange > DEBOUNCE_MS && reading != stableState) {
    stableState = reading;
    if (stableState == LOW && !pressLatched) {   // just pressed
      pressLatched = true;
      return true;
    }
    if (stableState == HIGH) pressLatched = false;  // released; arm next press
  }
  return false;
}

void handleInput() {
  bool tap = buttonTapped();
  if (!tap) return;

  Snake &me = snakes[mySlot];

  if (roundOver) {
    // Tap = "ready" vote for the next round.
    if (!me.ready) me.ready = true;
    return;
  }

  // In play: rotate the *pending* heading right. Start from the committed
  // heading so a double-tap in one tick can't fold us 180 into our own neck.
  int dx = me.dirX, dy = me.dirY;
  rotateRight(dx, dy);
  // Guard against reversal relative to committed heading (shouldn't happen from
  // a single 90 turn, but protects against odd states).
  if (!(dx == -me.dirX && dy == -me.dirY)) {
    me.pendingX = dx; me.pendingY = dy;
  }
}

// ============================================================================
//  Simulation — advance ONLY my snake; the opponent arrives via packets.
// ============================================================================
void stepMySnake() {
  Snake &me  = snakes[mySlot];
  Snake &opp = snakes[oppSlot];
  if (!me.alive) return;

  me.dirX = me.pendingX; me.dirY = me.pendingY;

  int nx = me.bodyX[me.head] + me.dirX;
  int ny = me.bodyY[me.head] + me.dirY;

  // Wall collision.
  if (nx < 0 || nx >= COLS || ny < 0 || ny >= ROWS) {
    me.alive = false; return;
  }

  bool eating = (nx == foodX && ny == foodY);

  // Self-collision (ignore own tail unless we're growing onto it).
  if (onSnakeBody(me, nx, ny, /*ignoreTail=*/!eating)) { me.alive = false; return; }

  // Collision with the OPPONENT's body. Ignore the opponent's tail too, since
  // in the same tick it is also vacating — this keeps head-to-tail grazes fair.
  if (onSnakeBody(opp, nx, ny, /*ignoreTail=*/true)) { me.alive = false; return; }

  // Advance.
  me.head = (me.head + 1) % MAXCELLS;
  me.bodyX[me.head] = nx;
  me.bodyY[me.head] = ny;

  if (eating) {
    if (me.length < MAXCELLS) me.length++;
    me.score++;
    // Only the HOST respawns food (authoritative). The client will keep eating
    // "air" until the host's new-food packet arrives, which is fine at these
    // speeds; the host almost always sees the eat first since it owns the food.
    if (amHost) hostSpawnFood();
  }
}

// Decide if the round is over (either snake dead) and who lost. Symmetric on
// both boards because both know both snakes' alive flags.
void evaluateRoundOver() {
  if (roundOver) return;
  bool a0 = snakes[0].valid ? snakes[0].alive : true;
  bool a1 = snakes[1].valid ? snakes[1].alive : true;
  if (!a0 || !a1) {
    roundOver = true;
    if (!a0 && !a1)      loserId = 2;   // draw
    else if (!a0)        loserId = 0;
    else                 loserId = 1;
    snakes[0].ready = false;
    snakes[1].ready = false;
  }
}

// ============================================================================
//  ESP-NOW send / receive
// ============================================================================
void buildPacket(SnakePacket &p) {
  Snake &me = snakes[mySlot];
  p.magic    = PROTO_MAGIC;
  p.ver      = PROTO_VER;
  p.playerId = myPlayerId;
  p.gameId   = gameId;
  p.cols     = COLS;
  p.rows     = ROWS;

  p.dirX  = me.dirX; p.dirY = me.dirY;
  p.score = me.score;
  p.alive = me.alive ? 1 : 0;
  p.ready = me.ready ? 1 : 0;

  p.foodX = foodX; p.foodY = foodY;   // meaningful from host; client echoes last
  p.foodSeq = foodSeq;

  // Serialize body oldest->newest (tail..head) so the receiver can rebuild the
  // ring buffer with head at len-1. Each cell packs to one byte: x*ROWS + y.
  int len = me.length;
  if (len > BODY_CAP) len = BODY_CAP;   // clamp (only reachable on a much larger panel)
  p.len = len;
  for (int i = 0; i < len; i++) {
    // walk from tail (offset len-1) toward head (offset 0)
    int idx = cellIndex(me, (me.length - 1) - i);
    p.cells[i] = (uint8_t)(me.bodyX[idx] * ROWS + me.bodyY[idx]);
  }
}

void sendState() {
  SnakePacket p;
  buildPacket(p);
  esp_now_send(BROADCAST_ADDR, (uint8_t *)&p, sizeof(p));
  lastSend = millis();
}

// Copy a received packet into the OPPONENT shadow snake + adopt shared/host data.
void applyPacket(const SnakePacket &p) {
  // The sender is the opponent (its playerId differs from ours). Guard anyway.
  if (p.playerId == myPlayerId) return;   // ignore our own echo (shouldn't happen)

  int slot = p.playerId;                  // 0 or 1
  if (slot < 0 || slot > 1) return;
  Snake &opp = snakes[slot];

  // --- Round sync -------------------------------------------------------
  // The HOST owns gameId. If we're the client and the host advanced the round,
  // adopt it (this is how "new round" propagates to the client).
  if (!amHost && p.playerId == 0 && p.gameId != gameId) {
    resetRound(p.gameId);
  }
  // Drop packets from an older round (stale) once we've matched ids.
  if (p.gameId != gameId) {
    // If WE are host and client is behind, ignore; our next packet fixes it.
    if (amHost) return;
  }

  // --- Rebuild opponent body from the packet ----------------------------
  int len = p.len;
  if (len < 1) len = 1;
  if (len > BODY_CAP) len = BODY_CAP;
  if (len > MAXCELLS) len = MAXCELLS;
  int senderRows = (p.rows > 0) ? p.rows : ROWS;   // decode key = sender's ROWS
  opp.length = len;
  opp.head = len - 1;                 // head is the last serialized cell
  for (int i = 0; i < len; i++) {
    uint8_t cell = p.cells[i];        // packed as x*rows + y
    opp.bodyX[i] = cell / senderRows;
    opp.bodyY[i] = cell % senderRows;
  }
  opp.dirX = p.dirX; opp.dirY = p.dirY;
  opp.score = p.score;
  opp.alive = (p.alive != 0);
  opp.ready = (p.ready != 0);
  opp.valid = true;

  // --- Shared food ------------------------------------------------------
  // Host is authoritative. If a packet from the host carries newer food, adopt.
  if (!amHost && p.playerId == 0) {
    if (p.foodSeq != foodSeq || foodX != p.foodX || foodY != p.foodY) {
      foodX = p.foodX; foodY = p.foodY; foodSeq = p.foodSeq;
    }
  }

  lastPeerMsg = millis();
  peerKnown = true;
}

// ESP-NOW receive callback.
// NOTE: signature matches espressif esp32 Arduino core v3.x (tested against
// 3.3.10): the first arg is `const esp_now_recv_info_t*` and the source MAC is
// info->src_addr. On older 2.x cores this was `const uint8_t *mac` instead — if
// you build against a 2.x core, change the signature and use `mac` directly.
void onDataRecv(const esp_now_recv_info_t *info, const uint8_t *data, int len) {
  if (len != (int)sizeof(SnakePacket)) return;
  SnakePacket p;
  memcpy(&p, data, sizeof(p));
  if (p.magic != PROTO_MAGIC || p.ver != PROTO_VER) return;

  // Learn the peer + (re)decide host role by MAC comparison the first time.
  if (!peerKnown) {
    memcpy(peerMac, info->src_addr, 6);
    decideRoleAgainstPeer(peerMac);
  }
  applyPacket(p);
}

// Optional TX status (useful when debugging over serial).
// v3.x core signature: (const wifi_tx_info_t *tx_info, esp_now_send_status_t).
// (On 2.x cores this was (const uint8_t *mac, esp_now_send_status_t).)
void onDataSent(const wifi_tx_info_t *tx_info, esp_now_send_status_t status) {
  (void)tx_info; (void)status;
}

// ============================================================================
//  Role election — lower MAC becomes host. Deterministic, no negotiation msgs.
// ============================================================================
int macCompare(const uint8_t *a, const uint8_t *b) {
  for (int i = 0; i < 6; i++) {
    if (a[i] < b[i]) return -1;
    if (a[i] > b[i]) return 1;
  }
  return 0;
}

void decideRoleAgainstPeer(const uint8_t *pm) {
  amHost = (macCompare(myMac, pm) < 0);
  myPlayerId = amHost ? 0 : 1;
  mySlot  = myPlayerId;
  oppSlot = 1 - myPlayerId;
  // We just learned our identity; make sure our own snake occupies the right
  // slot for the current round layout. Re-lay both from the current gameId so
  // positions are consistent. (Host keeps its food.)
  int savedScore = snakes[mySlot].score;   // preserve if mid-round (rare)
  resetRound(gameId);
  snakes[mySlot].score = savedScore;
}

// ============================================================================
//  Rendering — my snake filled, opponent outlined; shared food; HUD.
// ============================================================================
void drawSnakeFilled(const Snake &s) {
  for (int i = 0; i < s.length; i++) {
    int idx = cellIndex(s, i);
    u8g2.drawBox(s.bodyX[idx] * TILE, s.bodyY[idx] * TILE, TILE - 1, TILE - 1);
  }
}
void drawSnakeOutlined(const Snake &s) {
  for (int i = 0; i < s.length; i++) {
    int idx = cellIndex(s, i);
    u8g2.drawFrame(s.bodyX[idx] * TILE, s.bodyY[idx] * TILE, TILE - 1, TILE - 1);
  }
}

void render() {
  u8g2.clearBuffer();

  // Opponent first (outlined), then me (filled) on top so overlaps read as mine.
  if (snakes[oppSlot].valid) drawSnakeOutlined(snakes[oppSlot]);
  if (snakes[mySlot].valid)  drawSnakeFilled(snakes[mySlot]);

  // Shared food: small centered square.
  if (foodX >= 0) {
    u8g2.drawBox(foodX * TILE + 1, foodY * TILE + 1, TILE - 2, TILE - 2);
  }

  // HUD, tiny font in the top-left (matches snake.ino / blockdigger style).
  u8g2.setFont(u8g2_font_4x6_tf);
  u8g2.setCursor(0, 6);

  bool linkUp = peerKnown && (millis() - lastPeerMsg < PEER_TIMEOUT_MS);

  if (!peerKnown) {
    // Still searching for the other board.
    u8g2.print(amHost ? "H" : "?");
    u8g2.print(" link..");
  } else if (!linkUp) {
    u8g2.print("link lost");
  } else if (roundOver) {
    // Show who won + ready votes.
    const char *msg;
    if (loserId == 2)                 msg = "DRAW";
    else if (loserId == mySlot)       msg = "LOSE";
    else                              msg = "WIN ";
    u8g2.print(msg);
    u8g2.print(" ");
    u8g2.print(snakes[mySlot].score);
    // Ready indicators bottom-left: me then opp.
    u8g2.setCursor(0, ROWS * TILE);   // near the bottom of the grid area
    u8g2.print("tap:rdy ");
    u8g2.print(snakes[mySlot].ready ? "You" : "you");
    u8g2.print("/");
    u8g2.print(snakes[oppSlot].ready ? "Opp" : "opp");
  } else {
    // In play: my score, then opp score small.
    u8g2.print(snakes[mySlot].score);
    u8g2.print(":");
    u8g2.print(snakes[oppSlot].valid ? snakes[oppSlot].score : 0);
  }

  u8g2.sendBuffer();
}

// ============================================================================
//  Host round-restart arbitration
// ============================================================================
// When the round is over, the HOST decides when the next round begins: once
// both players have tapped "ready" (or after a grace timeout so a dead link
// doesn't stall forever), the host bumps gameId and resets. The client adopts
// the new gameId from the host's next packet.
uint32_t roundOverSince = 0;
void hostArbitrateRestart() {
  if (!amHost) return;
  if (!roundOver) { roundOverSince = 0; return; }
  if (roundOverSince == 0) roundOverSince = millis();

  bool bothReady = snakes[mySlot].ready && snakes[oppSlot].ready;
  bool graced    = (millis() - roundOverSince > 6000);   // 6s failsafe

  if (bothReady || graced) {
    resetRound((uint8_t)(gameId + 1));   // new round id -> client will follow
    sendState();                          // announce immediately
  }
}

// ============================================================================
//  Arduino entry points
// ============================================================================
void espnowSetup() {
  WiFi.mode(WIFI_STA);
  WiFi.disconnect();                 // not joining an AP; we only use ESP-NOW

  // Pin the radio to a fixed channel so both boards talk on the same one.
  esp_wifi_set_promiscuous(true);
  esp_wifi_set_channel(WIFI_CHANNEL, WIFI_SECOND_CHAN_NONE);
  esp_wifi_set_promiscuous(false);

  WiFi.macAddress(myMac);            // learn our own MAC for role election

  if (esp_now_init() != ESP_OK) {
    // Show the failure on-screen; without the radio the game can't sync.
    u8g2.clearBuffer();
    u8g2.setFont(u8g2_font_5x7_tf);
    u8g2.drawStr(0, 10, "ESP-NOW");
    u8g2.drawStr(0, 20, "init FAIL");
    u8g2.sendBuffer();
    return;
  }

  esp_now_register_recv_cb(onDataRecv);
  esp_now_register_send_cb(onDataSent);

  // Register the broadcast "peer" so esp_now_send() to FF:FF:.. is allowed.
  esp_now_peer_info_t peer = {};
  memcpy(peer.peer_addr, BROADCAST_ADDR, 6);
  peer.channel = WIFI_CHANNEL;
  peer.encrypt = false;
  peer.ifidx   = WIFI_IF_STA;
  esp_now_add_peer(&peer);
}

void setup() {
  Serial.begin(115200);

  pinMode(BTN_PIN, INPUT_PULLUP);    // BOOT button reads LOW when pressed

  Wire.begin(I2C_SDA, I2C_SCL);
  u8g2.begin();
  u8g2.setBusClock(400000);

  // Provisional layout before we know our role. We default to host (playerId 0)
  // until we hear a peer with a lower MAC; decideRoleAgainstPeer() fixes it.
  amHost = true; myPlayerId = 0; mySlot = 0; oppSlot = 1;
  snakes[0].valid = snakes[1].valid = false;
  resetRound(0);

  espnowSetup();

  lastStep = millis();
}

void loop() {
  handleInput();

  // Movement tick: advance my snake, re-evaluate round state, arbitrate restart.
  if (millis() - lastStep > STEP_MS) {
    lastStep = millis();

    if (!roundOver) {
      stepMySnake();
      evaluateRoundOver();
    }
    hostArbitrateRestart();

    // Broadcast our state once per tick (the heartbeat that drives sync).
    sendState();
  }

  // Also send a low-rate heartbeat between ticks so a freshly-booted peer hears
  // us quickly (and so "ready" votes propagate fast at game over).
  if (millis() - lastSend > 150) sendState();

  render();
  delay(16);   // ~60 fps render cap
}
