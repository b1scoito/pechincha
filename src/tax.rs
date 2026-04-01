use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::models::{TaxInfo, TaxRegime};

/// Brazilian import tax rates (as of 2024/2025)
const REMESSA_CONFORME_LOW_RATE: Decimal = dec!(0.20); // 20% on purchases < USD 50
const REMESSA_CONFORME_HIGH_RATE: Decimal = dec!(0.60); // 60% on purchases USD 50-3000
const REMESSA_CONFORME_HIGH_DEDUCTION_USD: Decimal = dec!(20.0); // USD 20 deduction on 60% bracket
const INTERNATIONAL_STANDARD_RATE: Decimal = dec!(0.60); // 60% for non-RC platforms
const ICMS_RATE: Decimal = dec!(0.17); // 17% ICMS (standardized)
const USD_50_THRESHOLD: Decimal = dec!(50.0);

pub struct TaxCalculator;

impl TaxCalculator {
    /// Calculate tax breakdown for a product.
    ///
    /// - `price_usd`: Product price in USD (for international products)
    /// - `price_brl`: Product price in BRL
    /// - `is_domestic`: Whether the product ships from within Brazil
    /// - `remessa_conforme`: Whether the platform participates in Remessa Conforme
    /// - `taxes_already_included`: Whether the platform already includes taxes in the displayed price
    /// - `exchange_rate`: Current USD/BRL exchange rate
    #[must_use]
    pub fn calculate(
        price_usd: Option<Decimal>,
        price_brl: Decimal,
        is_domestic: bool,
        remessa_conforme: bool,
        taxes_already_included: bool,
        exchange_rate: Decimal,
    ) -> TaxInfo {
        // Domestic products: taxes already baked into the price
        if is_domestic {
            return TaxInfo {
                remessa_conforme: false,
                taxes_included: true,
                import_tax: None,
                icms: None,
                total_tax: Decimal::ZERO,
                tax_regime: TaxRegime::Domestic,
            };
        }

        // If the platform already includes taxes (e.g., Shopee/AliExpress in RC),
        // we trust their price and don't add more taxes
        if taxes_already_included {
            let regime = if remessa_conforme {
                let usd_price = price_usd
                    .unwrap_or_else(|| price_brl / exchange_rate);
                if usd_price <= USD_50_THRESHOLD {
                    TaxRegime::RemessaConformeUnder50
                } else {
                    TaxRegime::RemessaConformeOver50
                }
            } else {
                TaxRegime::InternationalStandard
            };

            return TaxInfo {
                remessa_conforme,
                taxes_included: true,
                import_tax: None,
                icms: None,
                total_tax: Decimal::ZERO,
                tax_regime: regime,
            };
        }

        // Calculate taxes for international products where taxes are NOT included
        let usd_price = price_usd
            .unwrap_or_else(|| price_brl / exchange_rate);

        if remessa_conforme {
            if usd_price <= USD_50_THRESHOLD {
                // 20% import tax + 17% ICMS
                let import_tax = price_brl * REMESSA_CONFORME_LOW_RATE;
                let icms_base = price_brl + import_tax;
                // ICMS is calculated "por dentro" (from inside): base / (1 - rate) - base
                let icms = icms_base / (Decimal::ONE - ICMS_RATE) - icms_base;
                let total = import_tax + icms;

                TaxInfo {
                    remessa_conforme: true,
                    taxes_included: false,
                    import_tax: Some(import_tax),
                    icms: Some(icms),
                    total_tax: total,
                    tax_regime: TaxRegime::RemessaConformeUnder50,
                }
            } else {
                // 60% import tax (with USD 20 deduction) + 17% ICMS
                let deduction_brl = REMESSA_CONFORME_HIGH_DEDUCTION_USD * exchange_rate;
                let import_tax = (price_brl * REMESSA_CONFORME_HIGH_RATE - deduction_brl)
                    .max(Decimal::ZERO);
                let icms_base = price_brl + import_tax;
                let icms = icms_base / (Decimal::ONE - ICMS_RATE) - icms_base;
                let total = import_tax + icms;

                TaxInfo {
                    remessa_conforme: true,
                    taxes_included: false,
                    import_tax: Some(import_tax),
                    icms: Some(icms),
                    total_tax: total,
                    tax_regime: TaxRegime::RemessaConformeOver50,
                }
            }
        } else {
            // Non-Remessa Conforme international: 60% + 17% ICMS
            let import_tax = price_brl * INTERNATIONAL_STANDARD_RATE;
            let icms_base = price_brl + import_tax;
            let icms = icms_base / (Decimal::ONE - ICMS_RATE) - icms_base;
            let total = import_tax + icms;

            TaxInfo {
                remessa_conforme: false,
                taxes_included: false,
                import_tax: Some(import_tax),
                icms: Some(icms),
                total_tax: total,
                tax_regime: TaxRegime::InternationalStandard,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXCHANGE_RATE: Decimal = dec!(5.50); // approximate USD/BRL

    #[test]
    fn domestic_product_has_no_additional_tax() {
        let info = TaxCalculator::calculate(
            None,
            dec!(100.00),
            true,
            false,
            false,
            EXCHANGE_RATE,
        );
        assert_eq!(info.tax_regime, TaxRegime::Domestic);
        assert_eq!(info.total_tax, Decimal::ZERO);
    }

    #[test]
    fn remessa_conforme_under_50_usd() {
        // Product at USD 30 = BRL 165
        let price_brl = dec!(165.00);
        let info = TaxCalculator::calculate(
            Some(dec!(30.00)),
            price_brl,
            false,
            true,
            false,
            EXCHANGE_RATE,
        );
        assert_eq!(info.tax_regime, TaxRegime::RemessaConformeUnder50);
        assert!(info.import_tax.unwrap() > Decimal::ZERO);
        assert!(info.icms.unwrap() > Decimal::ZERO);
        // 20% import = 33.00, ICMS base = 198.00, ICMS "por dentro" ≈ 40.53
        // Total ≈ 73.53
        assert!(info.total_tax > dec!(70.0));
        assert!(info.total_tax < dec!(80.0));
    }

    #[test]
    fn remessa_conforme_over_50_usd() {
        // Product at USD 100 = BRL 550
        let price_brl = dec!(550.00);
        let info = TaxCalculator::calculate(
            Some(dec!(100.00)),
            price_brl,
            false,
            true,
            false,
            EXCHANGE_RATE,
        );
        assert_eq!(info.tax_regime, TaxRegime::RemessaConformeOver50);
        assert!(info.import_tax.unwrap() > Decimal::ZERO);
        // 60% of 550 = 330 - 110 (20 USD * 5.5) = 220
        let expected_import = dec!(220.00);
        assert_eq!(info.import_tax.unwrap(), expected_import);
    }

    #[test]
    fn taxes_already_included_returns_zero() {
        let info = TaxCalculator::calculate(
            Some(dec!(30.00)),
            dec!(165.00),
            false,
            true,
            true,
            EXCHANGE_RATE,
        );
        assert_eq!(info.total_tax, Decimal::ZERO);
        assert!(info.taxes_included);
    }
}
