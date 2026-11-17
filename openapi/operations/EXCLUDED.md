# Excluded operations

Operations found by the analysts but left out of the spec. One line each: id, then the reason.

- `alexaDiscovery` (`MessageAlexaDiscoveryRequest`): Alexa skill runtime call, sent by the TapHome Alexa backend, no configuration value.
- `alexaGetDeviceValues` (`MessageAlexaGetDeviceValuesRequest`): Alexa skill runtime call, sent by the TapHome Alexa backend, no configuration value.
- `alexaSetDevice` (`MessageAlexaSetDeviceRequest`): Alexa skill runtime call, sent by the TapHome Alexa backend, no configuration value.
- `doorbellP2PConnection` (`MessageDoorbellP2PConnectionRequest`): doorbell audio P2P handshake, media streaming.
- `doorbellState` (`MessageDoorbellStateRequest`): doorbell call control (answer, hang up, open door), day-to-day control.
- `downloadCommunityTemplates`: client-side only (no Core message); the app does it locally.
- `exportModbusTemplate`: client-side only (no Core message); the app does it locally.
- `exportPacketParserTemplate`: client-side only (no Core message); the app does it locally.
- `getImageData` (`MessageGetImageDataRequest`): doorbell camera snapshot, media.
- `integromatAttachWebhook` (`MessageIntegromatAttachWebhookRequest`): Integromat (Make) runtime call, sent by the TapHome cloud, no configuration value.
- `integromatDetachWebhook` (`MessageIntegromatDetachWebhookRequest`): Integromat (Make) runtime call, sent by the TapHome cloud, no configuration value.
- `integromatDiscovery` (`MessageIntegromatDiscoveryRequest`): Integromat (Make) runtime call, sent by the TapHome cloud, no configuration value.
- `integromatGetDeviceValue` (`MessageIntegromatGetDeviceValueRequest`): Integromat (Make) runtime call, sent by the TapHome cloud, no configuration value.
- `integromatGetDevices` (`MessageIntegromatGetDevicesRequest`): Integromat (Make) runtime call, sent by the TapHome cloud, no configuration value.
- `integromatSetDeviceValue` (`MessageIntegromatSetDeviceValueRequest`): Integromat (Make) runtime call, sent by the TapHome cloud, no configuration value.
- `integromatValueLogTypeDiscovery` (`MessageIntegromatValueLogTypeDiscoveryRequest`): Integromat (Make) runtime call, sent by the TapHome cloud, no configuration value.
- `loadLocalTemplateFile`: client-side only (no Core message); the app does it locally.
- `mediaDownload` (`MessageMediaDownloadRequest`): media library chunk transfer, no configuration value.
- `mediaUpload` (`MessageMediaUploadRequest`): media library chunk transfer, no configuration value.
- `sIPDoorbellPTZGotoPreset` (`MessageSIPDoorbellPTZGotoPresetRequest`): camera pan/tilt/zoom control, media.
- `sIPDoorbellPTZRelativeMove` (`MessageSIPDoorbellPTZRelativeMoveRequest`): camera pan/tilt/zoom control, media.
- `sIPDoorbellPTZStart` (`MessageSIPDoorbellPTZStartRequest`): camera pan/tilt/zoom control, media.
- `validateScript`: client-side only (no Core message); the app does it locally.

Notes:
- Entries without a Core message that only push broadcasts (kind `subscribe`) stay in the operation files and feed the `webhooks` section.
- Duplicate ids across the analyst manifests were merged into one entry, the longer description wins and evidence is combined. The legacy single-device `updateDeviceProperties` was dropped in favour of `updateDevicePropertiesSingle`, which uses the same message.
