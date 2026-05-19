---
id: REQ-WATERMARK-001
type: requirement
title: Auto watermark placement
status: active
---

# Auto watermark placement

用户导入图片后，系统应自动避开人脸区域放置水印。

## Related

- symbol://lib/domain/watermark/auto_placement_service.dart#AutoPlacementService
- test://test/watermark/auto_placement_service_test.dart#places-watermark-outside-face-region
