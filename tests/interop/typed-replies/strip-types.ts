import {
  CsilDecimal, RpErrorCode, RpErrorCodeNumbers, Status,
  fromReadingCbor, toReadingCbor, type Reading,
} from "./index.ts";

const code: RpErrorCode = RpErrorCode.ACCOUNT_RATE_LIMITED;
const reading: Reading = { code, status: Status.READY, price: new CsilDecimal(-2, 123n) };
const result = fromReadingCbor(toReadingCbor(reading));
if (result.code !== code || result.status !== "ready" || result.price.toString() !== "1.23")
  throw new Error("incorrect reading after CBOR round trip");
if (RpErrorCodeNumbers.numberFromEnum(code) !== 42n || RpErrorCodeNumbers.enumFromNumber(42n) !== code)
  throw new Error("incorrect named-number conversion");
