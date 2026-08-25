#!/usr/bin/env bash
# #413 fixture: the REAL defect — the publisher's roster is short esp32c5 (id 4). This is the
# state ota_publish.sh was actually in, where a valid C5 descriptor read as absent.
CHIPS = {1: "esp32c3", 2: "esp32c6", 3: "esp32s3"}
