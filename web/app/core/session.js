import {api} from './api.js';
import {S} from './store.js';
import {showLock} from '../ui/lock.js';

/* ============================================================ session */
function sessionGone(){if(!S.signedIn&&S.locked)return;S.signedIn=false;S.session=null;showLock('Signed out. Use a passkey to come back.')}
async function pollSession(){if(!S.signedIn)return;try{const r=await api('GET','/session',undefined,{quiet:true});S.session=r.session;S.live=!!r.live;if(r.session.locked&&!S.locked)showLock('Locked. Use your passkey to come back.')}catch(e){if(e.status===401)sessionGone()}}

export {sessionGone,pollSession};
