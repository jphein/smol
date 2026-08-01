// A ChipBudget written in a form the scraper does not recognise: the `chip:` field is
// built rather than a literal. The check must go RED, not silently see a smaller roster.
pub const ESP32C3: ChipBudget = ChipBudget { chip: "esp32c3" };
pub const MYSTERY: ChipBudget = ChipBudget { chip: CHIP_NAME_CONST };
