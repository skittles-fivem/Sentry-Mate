# CAN databases

Copied from Sentry Six Manager's `server/dbc/` (2026-11-16), so both programs decode a log the same way. Each
source keeps its own licence:

| File | Source | Licence |
|---|---|---|
| `Model3CAN.dbc` | [model3dbc](https://github.com/joshwardell/model3dbc), commit c56c113 | MIT, `LICENSE-model3dbc.txt` |
| `model3y-opendbc.dbc` | messages model3dbc lacks, from [opendbc](https://github.com/commaai/opendbc) commit 27fa1c5, checked on a Model Y | MIT, `LICENSE-opendbc.txt` |
| `opendbc-tesla_can.dbc`, `opendbc-tesla_powertrain.dbc` | opendbc, Model S/X before 2021 | MIT, `LICENSE-opendbc.txt` |
| `sx-legacy.dbc` | messages Open Vehicle Monitoring System's Model S module reads | MIT, `LICENSE-ovms.txt` |
| `override.dbc` | Sentry Six Manager: layouts that changed in newer Tesla firmware, checked on the owner's Model Y | the owner's own work |
