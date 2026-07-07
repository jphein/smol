// ============================================================================
//  smol watch — ESP32-C3 SuperMini + 0.42" OLED (72x40 SSD1306)
//  A starter smartwatch face: NTP clock + Open-Meteo weather over WiFi,
//  with a clearly-stubbed BLE-notifications section (NOT implemented — see below).
//
//  Board:   ESP32-C3 SuperMini (select "ESP32C3 Dev Module" in Arduino IDE)
//  Display: 0.42" OLED, SSD1306, 72x40 effective, I2C  SDA=GPIO5  SCL=GPIO6  addr 0x3C
//
//  LIBRARIES (install via Arduino Library Manager):
//    - U8g2        by oliver        (display)
//    - ArduinoJson by Benoit Blanchon (weather JSON parse)
//    WiFi.h / HTTPClient.h / WiFiClientSecure.h / time.h ship with the ESP32 core.
//
//  Toolchain: Arduino-ESP32 core. See README.md for the full toolchain rationale.
//
//  HONEST SCOPE: this compiles-plausibly and implements time + weather. It does
//  NOT implement notifications, calendar, alarms, or deep sleep — those are
//  scaffolded as TODOs so you can grow into them without ripping the base apart.
// ============================================================================

#include <U8g2lib.h>
#include <Wire.h>
#include <WiFi.h>
#include <WiFiClientSecure.h>
#include <HTTPClient.h>
#include <ArduinoJson.h>
#include <time.h>

// ----------------------------------------------------------------------------
//  USER CONFIG  — EDIT THESE
// ----------------------------------------------------------------------------
#define WIFI_SSID     "YOUR_WIFI_SSID"        // <-- CHANGE ME
#define WIFI_PASSWORD "YOUR_WIFI_PASSWORD"    // <-- CHANGE ME

// Your location for weather. Find yours at https://www.latlong.net/
// Defaults below point at Kansas City, MO (change to yours).
#define WEATHER_LAT   "39.0997"               // <-- CHANGE ME (latitude)
#define WEATHER_LON   "-94.5786"              // <-- CHANGE ME (longitude)

// Timezone. Uses a POSIX TZ string so DST is handled automatically.
// Examples:
//   US Central:  "CST6CDT,M3.2.0,M11.1.0"
//   US Eastern:  "EST5EDT,M3.2.0,M11.1.0"
//   US Pacific:  "PST8PDT,M3.2.0,M11.1.0"
//   UK:          "GMT0BST,M3.5.0/1,M10.5.0"
//   Central EU:  "CET-1CEST,M3.5.0,M10.5.0/3"
// Full list: https://github.com/nayarsystems/posix_tz_db/blob/master/zones.csv
#define TZ_STRING     "CST6CDT,M3.2.0,M11.1.0"   // <-- CHANGE ME if not US Central

// How often to refresh weather (ms). Be polite to the free API: 15 min is plenty.
static const unsigned long WEATHER_REFRESH_MS = 15UL * 60UL * 1000UL;

// ----------------------------------------------------------------------------
//  DISPLAY  — reuse the project constructor (see oled_test/). No offset needed.
// ----------------------------------------------------------------------------
U8G2_SSD1306_72X40_ER_F_HW_I2C u8g2(U8G2_R0, /*reset=*/U8X8_PIN_NONE);
static const int I2C_SDA = 5;
static const int I2C_SCL = 6;

// ----------------------------------------------------------------------------
//  STATE
// ----------------------------------------------------------------------------
static bool          g_timeSynced   = false;
static bool          g_weatherOK    = false;
static float         g_tempC        = 0.0f;
static int           g_weatherCode  = -1;
static bool          g_isDay        = true;
static unsigned long g_lastWeather  = 0;
static unsigned long g_lastDraw     = 0;

// ----------------------------------------------------------------------------
//  WiFi + NTP
// ----------------------------------------------------------------------------
void connectWiFi() {
  if (WiFi.status() == WL_CONNECTED) return;
  Serial.printf("WiFi: connecting to \"%s\"...\n", WIFI_SSID);
  WiFi.mode(WIFI_STA);
  WiFi.begin(WIFI_SSID, WIFI_PASSWORD);
  // Non-fatal: we time out after ~15 s and let loop() retry so the clock
  // still shows (stale) once time was synced at least once.
  unsigned long start = millis();
  while (WiFi.status() != WL_CONNECTED && millis() - start < 15000) {
    delay(250);
    Serial.print(".");
  }
  Serial.println();
  if (WiFi.status() == WL_CONNECTED) {
    Serial.printf("WiFi: connected, IP %s\n", WiFi.localIP().toString().c_str());
  } else {
    Serial.println("WiFi: FAILED (will retry). Check WIFI_SSID/WIFI_PASSWORD.");
  }
}

void syncTime() {
  if (WiFi.status() != WL_CONNECTED) return;
  // configTzTime applies the POSIX TZ (incl. DST) and starts SNTP.
  configTzTime(TZ_STRING, "pool.ntp.org", "time.nist.gov");
  Serial.print("NTP: syncing");
  struct tm t;
  // getLocalTime blocks up to the timeout waiting for the first SNTP packet.
  for (int i = 0; i < 20 && !getLocalTime(&t, 500); i++) Serial.print(".");
  Serial.println();
  if (getLocalTime(&t, 100)) {
    g_timeSynced = true;
    Serial.printf("NTP: %04d-%02d-%02d %02d:%02d:%02d\n",
                  t.tm_year + 1900, t.tm_mon + 1, t.tm_mday,
                  t.tm_hour, t.tm_min, t.tm_sec);
  } else {
    Serial.println("NTP: sync failed (will retry).");
  }
}

// ----------------------------------------------------------------------------
//  WEATHER  — Open-Meteo, free, NO API KEY.
//  Docs: https://open-meteo.com/en/docs
//  Example response:
//   { "current_weather": { "temperature":15.3, "weathercode":3, "is_day":1, ... } }
// ----------------------------------------------------------------------------
void fetchWeather() {
  if (WiFi.status() != WL_CONNECTED) return;

  String url = String("https://api.open-meteo.com/v1/forecast?latitude=")
             + WEATHER_LAT + "&longitude=" + WEATHER_LON
             + "&current_weather=true&temperature_unit=celsius";

  // Open-Meteo is HTTPS. setInsecure() skips cert validation — fine for a hobby
  // weather read. To pin properly, load Open-Meteo's root CA instead (see README).
  WiFiClientSecure client;
  client.setInsecure();

  HTTPClient https;
  if (!https.begin(client, url)) {
    Serial.println("Weather: https.begin() failed");
    return;
  }
  int code = https.GET();
  if (code != HTTP_CODE_OK) {
    Serial.printf("Weather: HTTP %d\n", code);
    https.end();
    return;
  }

  // Response is small (~250 bytes). Filter to just current_weather to save RAM.
  // ArduinoJson v7 API (Library Manager default). On v6, use
  // StaticJsonDocument<256> filter; / StaticJsonDocument<512> doc; instead.
  JsonDocument filter;
  filter["current_weather"] = true;

  JsonDocument doc;
  DeserializationError err =
      deserializeJson(doc, https.getStream(), DeserializationOption::Filter(filter));
  https.end();

  if (err) {
    Serial.printf("Weather: JSON error: %s\n", err.c_str());
    return;
  }

  JsonObject cw = doc["current_weather"];
  if (cw.isNull()) {
    Serial.println("Weather: no current_weather in response");
    return;
  }
  g_tempC       = cw["temperature"] | 0.0f;
  g_weatherCode = cw["weathercode"] | -1;
  g_isDay       = (cw["is_day"] | 1) != 0;
  g_weatherOK   = true;
  Serial.printf("Weather: %.1f C, code %d, %s\n",
                g_tempC, g_weatherCode, g_isDay ? "day" : "night");
}

// Map Open-Meteo WMO weather codes to a very short label that fits 72 px.
// Full table: https://open-meteo.com/en/docs (WMO Weather interpretation codes)
const char* weatherShort(int code) {
  if (code < 0)               return "--";
  if (code == 0)              return "Clear";
  if (code <= 3)              return "Cloudy";
  if (code == 45 || code == 48) return "Fog";
  if (code >= 51 && code <= 67) return "Rain";
  if (code >= 71 && code <= 77) return "Snow";
  if (code >= 80 && code <= 82) return "Showers";
  if (code >= 85 && code <= 86) return "Snow";
  if (code >= 95)             return "Storm";
  return "?";
}

// ----------------------------------------------------------------------------
//  BLE NOTIFICATIONS  — *** NOT IMPLEMENTED — INTENTIONALLY STUBBED ***
//
//  This is the hard part and there is no single cross-platform answer. Read the
//  README "Notifications" section before writing any of this. Summary:
//
//  iOS  (recommended, standards-based): Apple Notification Center Service (ANCS).
//    The watch acts as a BLE peripheral, advertises the ANCS *solicitation* UUID
//    (0x7905F431-B5CE-4E99-A40F-4B1E122D00D0), the iPhone connects & pairs, and
//    pushes notifications to you. Use the NimBLE-Arduino stack (h2zero/NimBLE-
//    Arduino) for low RAM. NOTE: as of 2025 there is an OPEN issue getting ANCS
//    advertising to work cleanly on the ESP32-C3 specifically (advertising the
//    solicited service UUID); budget real debugging time. Bonding must persist
//    to NVS so iOS reconnects. A known-working (non-C3) reference is the
//    Smartphone-Companions/ESP32-ANCS-Notifications Arduino library.
//
//  Android (NO standard equivalent to ANCS): you need a companion app to forward
//    notifications over BLE/GATT to a custom characteristic. Options:
//      (a) Gadgetbridge (open source) with a device profile, or
//      (b) a small custom Android app using NotificationListenerService.
//    The watch side would run a GATT server and render whatever it receives.
//
//  Because these paths differ per phone OS and ANCS-on-C3 is not turnkey, we do
//  NOT ship a fake implementation here. When ready, add a new file (e.g.
//  ble_ancs.cpp) and call bleNotificationsBegin()/bleNotificationsPoll() below.
// ----------------------------------------------------------------------------
void bleNotificationsBegin() {
  // TODO(ANCS/companion): initialize NimBLE, advertise ANCS solicitation UUID,
  // set up bonding (persist to NVS). See README "Notifications". No-op for now.
}
void bleNotificationsPoll() {
  // TODO: drain any received notifications and surface the latest on-screen.
}

// ----------------------------------------------------------------------------
//  UI  — 72x40 is tiny (~2,880 px). Keep it to a big clock + one weather line.
// ----------------------------------------------------------------------------
void drawScreen() {
  struct tm t;
  bool haveTime = g_timeSynced && getLocalTime(&t, 5);

  u8g2.clearBuffer();

  // --- Big HH:MM centered near the top ---
  char hhmm[6] = "--:--";
  if (haveTime) snprintf(hhmm, sizeof(hhmm), "%02d:%02d", t.tm_hour, t.tm_min);
  u8g2.setFont(u8g2_font_logisoso16_tn);   // 16px tall numeric font
  int w = u8g2.getStrWidth(hhmm);
  u8g2.drawStr((72 - w) / 2, 18, hhmm);

  // --- Date line (small) ---
  u8g2.setFont(u8g2_font_5x7_tf);
  if (haveTime) {
    char datebuf[16];
    // e.g. "Mon 07 Jul"
    strftime(datebuf, sizeof(datebuf), "%a %d %b", &t);
    int dw = u8g2.getStrWidth(datebuf);
    u8g2.drawStr((72 - dw) / 2, 28, datebuf);
  } else {
    const char* s = (WiFi.status() == WL_CONNECTED) ? "syncing..." : "no wifi";
    int dw = u8g2.getStrWidth(s);
    u8g2.drawStr((72 - dw) / 2, 28, s);
  }

  // --- Weather line at the bottom: "21C Cloudy" ---
  char wbuf[20];
  if (g_weatherOK) {
    snprintf(wbuf, sizeof(wbuf), "%dC %s", (int)lroundf(g_tempC), weatherShort(g_weatherCode));
  } else {
    snprintf(wbuf, sizeof(wbuf), "wx --");
  }
  int ww = u8g2.getStrWidth(wbuf);
  u8g2.drawStr((72 - ww) / 2, 38, wbuf);

  u8g2.sendBuffer();
}

// ----------------------------------------------------------------------------
//  SETUP / LOOP
// ----------------------------------------------------------------------------
void setup() {
  Serial.begin(115200);
  delay(300);
  Serial.println("\n=== smol watch starting ===");

  Wire.begin(I2C_SDA, I2C_SCL);
  u8g2.setBusClock(400000);
  u8g2.begin();

  // Splash so we know it's alive before WiFi finishes.
  u8g2.clearBuffer();
  u8g2.setFont(u8g2_font_6x10_tf);
  u8g2.drawStr(4, 14, "smol");
  u8g2.drawStr(4, 26, "watch");
  u8g2.sendBuffer();

  connectWiFi();
  syncTime();
  fetchWeather();
  g_lastWeather = millis();

  bleNotificationsBegin();   // no-op stub — see README
}

void loop() {
  unsigned long now = millis();

  // Keep WiFi/time alive; retry cheaply if we lost the network.
  if (WiFi.status() != WL_CONNECTED) {
    connectWiFi();
    if (WiFi.status() == WL_CONNECTED && !g_timeSynced) syncTime();
  }

  // Refresh weather on a timer.
  if (now - g_lastWeather >= WEATHER_REFRESH_MS) {
    fetchWeather();
    g_lastWeather = now;
  }

  bleNotificationsPoll();    // no-op stub — see README

  // Redraw ~1x/sec (RTC keeps time even if WiFi drops).
  if (now - g_lastDraw >= 1000) {
    drawScreen();
    g_lastDraw = now;
  }

  delay(20);   // TODO(power): replace busy loop with light/deep sleep — see README
}
