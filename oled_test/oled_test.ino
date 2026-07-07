// ============================================================================
//  PHASE-1 SANITY CHECK — ESP32-C3 SuperMini + 0.42" OLED
//  Confirms: (1) the OLED lights up with our constructor/pins, and
//            (2) prints every I2C device address over serial (should see 0x3C).
//  Flash this BEFORE the game to prove the display + toolchain work.
// ============================================================================
#include <U8g2lib.h>
#include <Wire.h>

U8G2_SSD1306_72X40_ER_F_HW_I2C u8g2(U8G2_R0, /*reset=*/U8X8_PIN_NONE);
static const int I2C_SDA = 5;
static const int I2C_SCL = 6;

int foundAddr = -1;

void scanI2C() {
  Serial.println("Scanning I2C bus (SDA=5, SCL=6)...");
  int count = 0;
  for (uint8_t a = 1; a < 127; a++) {
    Wire.beginTransmission(a);
    if (Wire.endTransmission() == 0) {
      Serial.printf("  device found at 0x%02X\n", a);
      if (foundAddr < 0) foundAddr = a;
      count++;
    }
  }
  Serial.printf("Scan done: %d device(s).\n", count);
}

void setup() {
  Serial.begin(115200);
  delay(300);
  Serial.println("\n=== ESP32-C3 OLED sanity check ===");

  Wire.begin(I2C_SDA, I2C_SCL);
  scanI2C();

  u8g2.setBusClock(400000);
  u8g2.begin();
  u8g2.clearBuffer();
  u8g2.setFont(u8g2_font_5x7_tf);
  u8g2.drawStr(0, 8,  "OLED OK!");
  u8g2.drawStr(0, 18, "72x40 :)");
  u8g2.setCursor(0, 30);
  if (foundAddr >= 0) { u8g2.print("I2C 0x"); u8g2.print(foundAddr, HEX); }
  else                  u8g2.print("no I2C?");
  // moving marker so you can see it's live, not frozen
  u8g2.drawFrame(0, 33, 72, 6);
  u8g2.sendBuffer();
}

void loop() {
  static int x = 0;
  u8g2.setDrawColor(0); u8g2.drawBox(1, 34, 70, 4); u8g2.setDrawColor(1);
  u8g2.drawBox(1 + x, 34, 6, 4);
  u8g2.sendBuffer();
  x = (x + 2) % 64;
  Serial.printf("alive, i2c=0x%02X\n", foundAddr);
  delay(120);
}
