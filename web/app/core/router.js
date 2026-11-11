import {S} from './store.js';
import {enterWall,exitWall} from '../ui/dock.js';
import {setView} from '../ui/views.js';

function route(){const h=location.hash;if(h==='#wall'){if(!S.wall)enterWall()}else{if(S.wall)exitWall(true);setView(h==='#rooms'?'rooms':'fav')}}

export {route};
