# Expression language -- function reference

All functions propagate null (return `Value::Null` if any argument is null) unless noted otherwise.

## Conditional

| Function | Signature | Description |
|---|---|---|
| `if` | `(Bool, T, T) -> T` | If condition true return second arg, else third |
| `multiIf` | `(Bool, T, Bool, T, ..., T) -> T` | Chained if/elseif/else (odd number of args) |
| `ifNull` | `(T?, T) -> T` | Return first arg if non-null, else second |
| `nullIf` | `(T, T) -> T?` | Return null if first equals second, else first |
| `coalesce` | `(T?...) -> T?` | First non-null argument (variadic) |
| `isNull` | `(any) -> Bool` | True if argument is null (never returns null) |
| `isNotNull` | `(any) -> Bool` | True if argument is not null (never returns null) |

## String

| Function | Signature | Description |
|---|---|---|
| `concat` | `(Text...) -> Text` | Concatenate all arguments (variadic, auto-formats non-text) |
| `length` | `(Text) -> Int64` | Character count |
| `substring` | `(Text, Int64, Int64?) -> Text` | Extract substring (start, optional length) |
| `charAt` | `(Text, Int64) -> Text?` | Character at index (null if out of bounds) |
| `upper` | `(Text) -> Text` | Uppercase |
| `lower` | `(Text) -> Text` | Lowercase |
| `trim` | `(Text) -> Text` | Strip leading/trailing whitespace |
| `replace` | `(Text, Text, Text) -> Text` | Replace all occurrences |
| `startsWith` | `(Text, Text) -> Bool` | True if string starts with prefix |
| `endsWith` | `(Text, Text) -> Bool` | True if string ends with suffix |
| `contains` | `(Text, Text) -> Bool` | True if string contains substring |
| `indexOf` | `(Text, Text) -> Int64` | Index of substring (-1 if not found) |
| `format` | `(Text, any...) -> Text` | Format string with positional `{0}`, `{1}` placeholders |
| `toString` | `(any) -> Text` | Convert any value to its text representation |
| `reverse` | `(Text) -> Text` | Reverse characters |
| `repeat` | `(Text, Int64) -> Text` | Repeat string N times |
| `leftPad` | `(Text, Int64, Text?) -> Text` | Pad left to target length |
| `rightPad` | `(Text, Int64, Text?) -> Text` | Pad right to target length |

## Arithmetic

| Function | Signature | Description |
|---|---|---|
| `add` | `(numeric, numeric) -> numeric` | Addition (also works on Text for concat) |
| `subtract` | `(numeric, numeric) -> numeric` | Subtraction |
| `multiply` | `(numeric, numeric) -> numeric` | Multiplication |
| `divide` | `(numeric, numeric) -> numeric` | Division (error on zero) |
| `modulo` | `(numeric, numeric) -> numeric` | Remainder (error on zero) |
| `negate` | `(numeric) -> numeric` | Unary negation |
| `abs` | `(numeric) -> numeric` | Absolute value |
| `ceil` | `(numeric) -> Int64` | Ceiling (round up) |
| `floor` | `(numeric) -> Int64` | Floor (round down) |
| `round` | `(numeric) -> Int64` | Round to nearest integer |
| `min` | `(numeric...) -> numeric?` | Minimum of all non-null args (variadic) |
| `max` | `(numeric...) -> numeric?` | Maximum of all non-null args (variadic) |
| `sign` | `(numeric) -> Int64` | Sign: -1, 0, or 1 |
| `power` | `(numeric, numeric) -> Float64` | Exponentiation |
| `sqrt` | `(numeric) -> Float64` | Square root |

Integer overflow promotes to `BigInt` automatically.

## Math

| Function | Signature | Description |
|---|---|---|
| `sin` | `(numeric) -> Float64` | Sine (radians) |
| `cos` | `(numeric) -> Float64` | Cosine |
| `tan` | `(numeric) -> Float64` | Tangent |
| `asin` | `(numeric) -> Float64` | Arc sine |
| `acos` | `(numeric) -> Float64` | Arc cosine |
| `atan` | `(numeric) -> Float64` | Arc tangent |
| `atan2` | `(numeric, numeric) -> Float64` | Two-argument arc tangent |
| `sinh` | `(numeric) -> Float64` | Hyperbolic sine |
| `cosh` | `(numeric) -> Float64` | Hyperbolic cosine |
| `tanh` | `(numeric) -> Float64` | Hyperbolic tangent |
| `asinh` | `(numeric) -> Float64` | Inverse hyperbolic sine |
| `acosh` | `(numeric) -> Float64` | Inverse hyperbolic cosine |
| `atanh` | `(numeric) -> Float64` | Inverse hyperbolic tangent |
| `log` | `(numeric) -> Float64` | Natural logarithm |
| `log2` | `(numeric) -> Float64` | Base-2 logarithm |
| `log10` | `(numeric) -> Float64` | Base-10 logarithm |
| `exp` | `(numeric) -> Float64` | e^x |
| `cbrt` | `(numeric) -> Float64` | Cube root |
| `pi` | `() -> Float64` | Pi constant |
| `e` | `() -> Float64` | Euler's number |
| `phi` | `() -> Float64` | Golden ratio |
| `tau` | `() -> Float64` | Tau (2*pi) |
| `erf` | `(numeric) -> Float64` | Error function |
| `erfc` | `(numeric) -> Float64` | Complementary error function |
| `gamma` | `(numeric) -> Float64` | Gamma function |
| `lnGamma` | `(numeric) -> Float64` | Log-gamma function |
| `beta` | `(numeric, numeric) -> Float64` | Beta function |
| `lambertW` | `(numeric) -> Float64` | Lambert W function (principal branch) |
| `isNaN` | `(numeric) -> Bool` | True if value is NaN |
| `isInfinite` | `(numeric) -> Bool` | True if value is infinite |
| `clamp` | `(numeric, numeric, numeric) -> Float64` | Clamp value between min and max |

## Comparison

| Function | Signature | Description |
|---|---|---|
| `equals` | `(T, T) -> Bool` | Equal (same type category required) |
| `notEquals` | `(T, T) -> Bool` | Not equal |
| `greater` | `(T, T) -> Bool` | Greater than |
| `less` | `(T, T) -> Bool` | Less than |
| `greaterOrEquals` | `(T, T) -> Bool` | Greater than or equal |
| `lessOrEquals` | `(T, T) -> Bool` | Less than or equal |

Comparable type categories: numeric, text, bool, date, timestamp, uuid.

## Logical

| Function | Signature | Description |
|---|---|---|
| `and` | `(Bool, Bool) -> Bool` | Logical AND |
| `or` | `(Bool, Bool) -> Bool` | Logical OR |
| `not` | `(Bool) -> Bool` | Logical NOT |

## Cast

| Function | Signature | Description |
|---|---|---|
| `toStringCast` | `(any) -> Text` | Convert any type to text |
| `toInt8` | `(numeric/Text/Bool) -> Int8` | Narrowing cast (error if out of range) |
| `toInt16` | `(numeric/Text/Bool) -> Int16` | Narrowing cast |
| `toInt32` | `(numeric/Text/Bool) -> Int32` | Narrowing cast |
| `toInt64` | `(numeric/Text/Bool/Float64) -> Int64` | Cast to Int64 |
| `toUInt8` | `(numeric/Text/Bool) -> UInt8` | Unsigned narrowing cast |
| `toUInt16` | `(numeric/Text/Bool) -> UInt16` | Unsigned narrowing cast |
| `toUInt32` | `(numeric/Text/Bool) -> UInt32` | Unsigned narrowing cast |
| `toUInt64` | `(numeric/Text/Bool/Int64/Float64) -> UInt64` | Cast to UInt64 |
| `toFloat32` | `(numeric/Text) -> Float32` | Cast to Float32 |
| `toFloat64` | `(numeric/Text) -> Float64` | Cast to Float64 |
| `toBool` | `(Int64/Text/Bool) -> Bool` | Cast to Bool (uses `bool_flag::parse`) |
| `toDate` | `(Text/Timestamp/Date) -> Date` | Parse YYYY-MM-DD or extract date |
| `toTimestamp` | `(Text/Int64/Date/Timestamp) -> Timestamp` | Parse RFC3339, unix seconds, or promote Date |
| `toUuid` | `(Text/Uuid) -> Uuid` | Parse UUID string |
| `toBigInt` | `(Int64/Text/BigInt) -> BigInt` | Cast to arbitrary-precision integer |
| `toDecimal` | `(numeric/Text, Int64, Int64) -> Decimal` | Cast with (precision, scale) validation |

## Datetime

| Function | Signature | Description |
|---|---|---|
| `now` | `() -> Timestamp` | Current UTC timestamp (from EvalContext) |
| `today` | `() -> Date` | Current UTC date |
| `second` | `(Timestamp) -> Int64` | Extract second (0-59) |
| `minute` | `(Timestamp) -> Int64` | Extract minute (0-59) |
| `hour` | `(Timestamp) -> Int64` | Extract hour (0-23) |
| `day` | `(Timestamp) -> Int64` | Extract day of month (1-31) |
| `month` | `(Timestamp) -> Int64` | Extract month (1-12) |
| `year` | `(Timestamp) -> Int64` | Extract year |
| `millisecond` | `(Timestamp) -> Int64` | Extract millisecond within second (0-999) |
| `dayOfWeek` | `(Timestamp) -> Int64` | Day of week (Monday=1, Sunday=7) |
| `dayOfYear` | `(Timestamp) -> Int64` | Day of year (1-366) |
| `toSeconds` | `(Timestamp) -> Int64` | Unix seconds |
| `toMillis` | `(Timestamp) -> Int64` | Unix milliseconds |
| `fromSeconds` | `(Int64) -> Timestamp` | Timestamp from unix seconds |
| `fromMillis` | `(Int64) -> Timestamp` | Timestamp from unix milliseconds |
| `addDays` | `(Timestamp, Int64) -> Timestamp` | Add N days |
| `addHours` | `(Timestamp, Int64) -> Timestamp` | Add N hours |
| `addMinutes` | `(Timestamp, Int64) -> Timestamp` | Add N minutes |
| `addSeconds` | `(Timestamp, Int64) -> Timestamp` | Add N seconds |
| `addMilliseconds` | `(Timestamp, Int64) -> Timestamp` | Add N milliseconds |
| `addMonths` | `(Timestamp, Int64) -> Timestamp` | Add N months (calendar) |
| `addYears` | `(Timestamp, Int64) -> Timestamp` | Add N years (calendar) |
| `subtractDays` | `(Timestamp, Int64) -> Timestamp` | Subtract N days |
| `subtractHours` | `(Timestamp, Int64) -> Timestamp` | Subtract N hours |
| `subtractMinutes` | `(Timestamp, Int64) -> Timestamp` | Subtract N minutes |
| `subtractSeconds` | `(Timestamp, Int64) -> Timestamp` | Subtract N seconds |
| `subtractMilliseconds` | `(Timestamp, Int64) -> Timestamp` | Subtract N milliseconds |
| `subtractMonths` | `(Timestamp, Int64) -> Timestamp` | Subtract N months (calendar) |
| `subtractYears` | `(Timestamp, Int64) -> Timestamp` | Subtract N years (calendar) |
| `dateDiff` | `(Timestamp, Timestamp) -> Int64` | Difference in seconds |
| `formatDateTime` | `(Timestamp, Text) -> Text` | Format with strftime pattern |

## Bytes

| Function | Signature | Description |
|---|---|---|
| `byteLength` | `(Bytes) -> Int64` | Byte count |
| `byteAt` | `(Bytes, Int64) -> Int64?` | Byte at index (null if out of bounds) |
| `byteSlice` | `(Bytes, Int64, Int64?) -> Bytes` | Slice from start with optional length |
| `bytesFromHex` | `(Text) -> Bytes` | Decode hex string to bytes |
| `bytesFromBase64` | `(Text) -> Bytes` | Decode standard base64 to bytes |
| `bytesFromUtf8` | `(Text) -> Bytes` | UTF-8 encode text to bytes |
| `hex` | `(Bytes) -> Text` | Encode bytes as hex string |
| `base64` | `(Bytes) -> Text` | Encode bytes as standard base64 |
| `base64Url` | `(Bytes) -> Text` | Encode bytes as URL-safe base64 (no padding) |
| `bytesEqual` | `(Bytes, Bytes) -> Bool` | Byte-wise equality |
| `urlEncode` | `(Text) -> Text` | Percent-encode for URLs |
| `urlDecode` | `(Text) -> Text` | Percent-decode |

## Encoding

| Function | Signature | Description |
|---|---|---|
| `encode` | `(Bytes, Text) -> Text` | Encode bytes with named encoding (hex, base64, base64url) |
| `decode` | `(Text, Text) -> Bytes` | Decode text with named encoding |
| `encodeText` | `(Text, Text) -> Bytes` | Encode text to bytes with charset (utf-8, latin1, etc.) |
| `decodeText` | `(Bytes, Text) -> Text` | Decode bytes to text with charset |

## Crypto

| Function | Signature | Description |
|---|---|---|
| `md5` | `(Text/Bytes) -> Text` | MD5 hash (hex) |
| `sha1` | `(Text/Bytes) -> Text` | SHA-1 hash (hex) |
| `sha256` | `(Text/Bytes) -> Text` | SHA-256 hash (hex) |
| `sha512` | `(Text/Bytes) -> Text` | SHA-512 hash (hex) |
| `xxHash64` | `(Text/Bytes) -> Int64` | xxHash64 |
| `xxHash32` | `(Text/Bytes) -> Int64` | xxHash32 |
| `cityHash64` | `(Text/Bytes) -> Int64` | CityHash64 (ClickHouse-compatible, uses `ch_cityhash102` crate) |
| `hmac` | `(Text, Text/Bytes, Text) -> Text` | HMAC (algorithm, data, key) -- hex output |

## JSON

| Function | Signature | Description |
|---|---|---|
| `parseJson` | `(Text) -> Json` | Parse JSON string |
| `toJson` | `(any) -> Text` | Serialize value to JSON string |
| `jsPath` | `(Json/Object, Text) -> Json?` | JSONPath query, first result |
| `jsPathString` | `(Json/Object, Text) -> Text?` | JSONPath query, first result as Text |
| `jsPathInt` | `(Json/Object, Text) -> Int64?` | JSONPath query, first result as Int64 |
| `jsPathFloat` | `(Json/Object, Text) -> Float64?` | JSONPath query, first result as Float64 |
| `jsPathBool` | `(Json/Object, Text) -> Bool?` | JSONPath query, first result as Bool |
| `jsonLength` | `(Json) -> Int64` | Length of JSON array or object |

## Object

| Function | Signature | Description |
|---|---|---|
| `objectLength` | `(Object) -> Int64` | Number of key-value pairs |
| `objectKeys` | `(Object) -> Json` | JSON array of keys |
| `objectValues` | `(Object) -> Json` | JSON array of values |
| `objectHasKey` | `(Object, Text) -> Bool` | True if key exists |
| `objectGet` | `(Object, Text) -> any?` | Get value by key (null if missing) |

## Random

| Function | Signature | Description |
|---|---|---|
| `randomUuid` | `() -> Uuid` | Generate UUID v4 |
| `randomInt` | `(Int64, Int64) -> Int64` | Random integer in [min, max) |
| `randomFloat` | `() -> Float64` | Random float in [0.0, 1.0) |
| `randomAlphanumeric` | `(Int64) -> Text` | Random alphanumeric string (max 1024 chars) |
| `randomHex` | `(Int64) -> Text` | Random hex string (max 1024 chars) |
| `randomBytes` | `(Int64) -> Bytes` | Random bytes (max 1 MiB) |
| `randomChoice` | `(any...) -> any` | Pick one argument at random (variadic) |

## Bitwise

| Function | Signature | Description |
|---|---|---|
| `bitAnd` | `(Int64, Int64) -> Int64` | Bitwise AND |
| `bitOr` | `(Int64, Int64) -> Int64` | Bitwise OR |
| `bitXor` | `(Int64, Int64) -> Int64` | Bitwise XOR |
| `bitNot` | `(Int64) -> Int64` | Bitwise NOT (complement) |
| `bitShiftLeft` | `(Int64, Int64) -> Int64` | Left shift (0-63) |
| `bitShiftRight` | `(Int64, Int64) -> Int64` | Arithmetic right shift (0-63) |
| `bitCount` | `(Int64) -> Int64` | Population count (number of set bits) |

## Env

| Function | Signature | Description |
|---|---|---|
| `env` | `(Text) -> Text?` | Read environment variable (null if missing) |
| `env` | `(Text, Text) -> Text` | Read environment variable with default |

## Misc

| Function | Signature | Description |
|---|---|---|
| `typeof` | `(any) -> Text` | Runtime type name (never returns null) |
