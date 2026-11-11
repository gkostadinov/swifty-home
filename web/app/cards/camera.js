import {$,esc,ICON} from '../core/util.js';
import {NR,T,TGT,nameOf} from '../core/store.js';
import {row} from '../ui/sheets.js';
import {placeRow} from '../pages/rooms.js';
import {notesBlock,wireNotes} from './index.js';

/* ============================================================ the camera: whether it answers. It never fetches a picture */
const cameraSheets={
  camera(){
    const word=()=>T.camKnown?(T.camOk?'Answering':'Not answering'):NR;const why=()=>T.camKnown?'This checks that the camera answers. It does not fetch a picture.':(T.camReason||'');
    const html=`<div class="rowi">${ICON('camera')}<div class="t"><b id="cam-word">${esc(word())}</b><small id="cam-age">${esc(why())}</small></div></div>
      <div class="rows">${row(null,'Picture quality','Last minute, megabits per second',NR)}</div>
      ${notesBlock(TGT.camera)}
      ${placeRow(TGT.camera)}`;
    return {title:TGT.camera?nameOf(TGT.camera,'Camera'):'Camera',html,mount(root){wireNotes(root)},update(){const root=$('#sbody');const w=$('#cam-word',root);if(!w)return;w.textContent=word();$('#cam-age',root).textContent=why()}};
  },
};

export {cameraSheets};
