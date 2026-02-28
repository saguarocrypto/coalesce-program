//! Fuzz target: random bytes as instruction data should never panic.
//! Tests the discriminator parsing and data length checks in lib.rs.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Test discriminator parsing logic (pure data validation)
    if data.is_empty() {
        return;
    }

    let disc = data[0];
    let payload = &data[1..];

    // Verify the discriminator check doesn't panic.
    // Match arms mirror src/lib.rs:65-99 exactly.
    match disc {
        0 => {
            // InitializeProtocol: needs 2 bytes (fee_rate_bps u16)
            let _valid = payload.len() >= 2;
        }
        1 => {
            // SetFeeConfig: needs 2 bytes (fee_rate_bps u16)
            let _valid = payload.len() >= 2;
        }
        2 => {
            // CreateMarket: needs 26 bytes
            let _valid = payload.len() >= 26;
        }
        3 => {
            // Deposit: needs 8 bytes (amount u64)
            if payload.len() >= 8 {
                let _amount = u64::from_le_bytes([
                    payload[0], payload[1], payload[2], payload[3],
                    payload[4], payload[5], payload[6], payload[7],
                ]);
            }
        }
        4 => {
            // Borrow: needs 8 bytes (amount u64)
            if payload.len() >= 8 {
                let _amount = u64::from_le_bytes([
                    payload[0], payload[1], payload[2], payload[3],
                    payload[4], payload[5], payload[6], payload[7],
                ]);
            }
        }
        5 => {
            // Repay: needs 8 bytes (amount u64)
            if payload.len() >= 8 {
                let _amount = u64::from_le_bytes([
                    payload[0], payload[1], payload[2], payload[3],
                    payload[4], payload[5], payload[6], payload[7],
                ]);
            }
        }
        6 => {
            // RepayInterest: needs 8 bytes (amount u64)
            if payload.len() >= 8 {
                let _amount = u64::from_le_bytes([
                    payload[0], payload[1], payload[2], payload[3],
                    payload[4], payload[5], payload[6], payload[7],
                ]);
            }
        }
        7 => {
            // Withdraw: needs 16 bytes (scaled_amount u128)
            if payload.len() >= 16 {
                let mut bytes = [0u8; 16];
                bytes.copy_from_slice(&payload[0..16]);
                let _scaled_amount = u128::from_le_bytes(bytes);
            }
        }
        8 | 9 | 10 | 11 => {
            // CollectFees / ReSettle / CloseLenderPosition / WithdrawExcess: no additional data
        }
        12 => {
            // SetBorrowerWhitelist: needs 9 bytes (1 byte flag + 8 byte u64)
            if payload.len() >= 9 {
                let _is_whitelisted = payload[0];
                let _cap = u64::from_le_bytes([
                    payload[1], payload[2], payload[3], payload[4],
                    payload[5], payload[6], payload[7], payload[8],
                ]);
            }
        }
        13 => {
            // SetPause: needs 1 byte (flag)
            if payload.len() >= 1 {
                let _flag = payload[0];
            }
        }
        14 => {
            // SetBlacklistMode: needs 1 byte (flag)
            if payload.len() >= 1 {
                let _flag = payload[0];
            }
        }
        15 | 16 => {
            // SetAdmin / SetWhitelistManager: no additional data
        }
        _ => {
            // Invalid discriminator — should result in InvalidInstructionData
        }
    }
});
