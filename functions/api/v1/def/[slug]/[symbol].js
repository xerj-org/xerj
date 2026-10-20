// GET /api/v1/def/<slug>/<symbol> — see functions/_lib/hub-hosted.js
import { hosted } from '../../../../_lib/hub-hosted.js';
export const onRequest = hosted('def');
