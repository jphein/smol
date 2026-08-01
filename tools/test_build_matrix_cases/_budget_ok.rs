pub struct ChipBudget { pub chip: &'static str }
impl ChipBudget { pub const fn x() {} }
pub const ESP32C3: ChipBudget = ChipBudget { chip: "esp32c3" };
pub const CHIP: ChipBudget = ChipBudget { chip: "host" };
