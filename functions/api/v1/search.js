// GET /api/v1/search?q=… — see functions/_lib/hub-hosted.js
import { hosted } from '../../_lib/hub-hosted.js';
export const onRequest = hosted('search');
