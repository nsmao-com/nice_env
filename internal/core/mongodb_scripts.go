package core

// Fixed MongoDB Shell programs ported from the existing native backend.
// Request values and credentials enter through stdin, never script interpolation.

const mongoConnectScript = `
try {
  const connection = new Mongo(input.uri + '/?directConnection=true&serverSelectionTimeoutMS=4000&socketTimeoutMS=6000&appName=NiceEnv');
  const admin = connection.getDB('admin');
  const checked = result => { if (result.ok !== 1) throw Object.assign(new Error(result.errmsg || 'MongoDB query failed'), { code: result.code }); return result; };
  if (input.credentials.username && !connection.getDB(input.credentials.authDatabase).auth({user:input.credentials.username,pwd:input.credentials.password})) {
    throw Object.assign(new Error('Authentication failed'),{code:18});
  }
  const options = checked(admin.runCommand({getCmdLineOpts:1}));
  const fs = require('fs');
  const normalize = path => { const resolved = fs.realpathSync(path); return process.platform === 'win32' ? resolved.toLowerCase() : resolved; };
  let sameDataDir = false;
  try { sameDataDir = !!options.parsed?.storage?.dbPath && normalize(options.parsed.storage.dbPath) === normalize(input.dataDir); } catch {}
  if (!sameDataDir) {
    throw Object.assign(new Error('MongoDB data directory changed'), { code: 'MONGO_INSTANCE_CHANGED' });
  }
`

const mongoBrowseScript = `
  const request = input.request;
  let result;
  if (request.action === 'overview') {
    const reply = checked(admin.runCommand({listDatabases:1,nameOnly:true,authorizedDatabases:true,maxTimeMS:5000}));
    const names = reply.databases.map(row => row.name).sort();
    result = {kind:'overview', version:input.version, serverVersion:admin.version(), port:input.port, uri:input.uri,
      shellVersion:input.shellVersion, databases:names.slice(0,1000), limited:names.length>1000};
  } else {
    const database = connection.getDB(request.database);
    const escaped = value => value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    const match = request.action === 'collections' ? (request.search ? {name:{$regex:escaped(request.search),$options:'i'}} : {}) : {name:request.collection};
    let cursor = checked(database.runCommand({listCollections:1,filter:match,nameOnly:true,authorizedCollections:true,cursor:{batchSize:100},maxTimeMS:5000})).cursor;
    const entries = [];
    let limited = false;
    try {
      while (true) {
        for (const item of cursor.firstBatch || cursor.nextBatch || []) {
          if (entries.length >= 1000) { limited = true; break; }
          entries.push({name:item.name,kind:item.type});
        }
        if (limited || cursor.id.toString() === '0') break;
        if (entries.length >= 1000) { limited = true; break; }
        cursor = checked(database.runCommand({getMore:cursor.id,collection:cursor.ns.slice(request.database.length+1),batchSize:100})).cursor;
      }
    } finally { if (cursor.id.toString() !== '0') database.runCommand({killCursors:cursor.ns.slice(request.database.length+1),cursors:[cursor.id]}); }
    if (request.action === 'collections') {
      entries.sort((a,b) => a.name.localeCompare(b.name)); result = {kind:'collections',database:request.database,entries,limited};
    } else {
      const collectionInfo = entries.find(entry => entry.name === request.collection);
      if (!collectionInfo) throw Object.assign(new Error('Collection no longer exists'), {code:'MONGO_COLLECTION_MISSING'});
      if (request.action === 'deleteDocument') {
        if (collectionInfo.kind !== 'collection') throw Object.assign(new Error('This collection is read-only'), {code:'MONGO_COLLECTION_READONLY'});
        let id;
        try { id = EJSON.parse(request.id, {relaxed:false}); } catch { throw Object.assign(new Error('Document id is invalid'), {code:'MONGO_DOCUMENT_INVALID'}); }
        const deleted = database.getCollection(request.collection).deleteOne({_id:id}, {maxTimeMS:5000});
        if (!deleted.acknowledged) throw Object.assign(new Error('Delete was not acknowledged'), {code:'MONGO_DELETE_UNACKNOWLEDGED'});
        if (deleted.deletedCount !== 1) throw Object.assign(new Error('Document no longer exists'), {code:'MONGO_DOCUMENT_MISSING'});
        result = {kind:'deleteDocument',database:request.database,collection:request.collection,deleted:deleted.deletedCount};
      } else {
      let filter = {};
      if (request.filter) {
        const spec = request.filter;
        const value = spec.valueType === 'number' ? Number(spec.value) : spec.valueType === 'boolean' ? spec.value === 'true' : spec.valueType === 'null' ? null : spec.valueType === 'objectId' ? ObjectId(spec.value) : spec.value;
        filter = {[spec.field]: {$eq:value}};
      }
      const docs = database.getCollection(request.collection).find(filter).sort({_id:1}).skip(request.offset).limit(request.limit+1).maxTimeMS(5000);
      const documents = [];
      let hasMore = false;
      try {
        while (docs.hasNext()) {
          if (documents.length >= request.limit) { hasMore = true; break; }
           const document = docs.next();
           const id = EJSON.stringify(document._id, null, 0, {relaxed:false});
           const text = EJSON.stringify(document, null, 2, {relaxed:false});
           documents.push({id,content:text.slice(0,65536),truncated:text.length>65536});
        }
      } finally { docs.close(); }
       result = {kind:'documents',database:request.database,collection:request.collection,offset:request.offset,limit:request.limit,documents,hasMore};
      }
    }
  }
  print(JSON.stringify({result}));
`

const mongoCatchScript = `
} catch (error) {
  print(JSON.stringify({error:{code:String(error.code || ''),message:String(error.message || 'MongoDB query failed').slice(0,2000)}}));
}
`

const mongoProbeScript = `
  const auth = checked(admin.runCommand({connectionStatus:1})).authInfo;
  let hasUsers = null;
  try { hasUsers = checked(admin.runCommand({usersInfo:{forAllDBs:true},showCredentials:false})).users.length > 0; } catch (error) { if (error.code !== 13) throw error; }
  print(JSON.stringify({result:{authorization:options.parsed?.security?.authorization === 'enabled',hasUsers,
    administrator:(auth.authenticatedUserRoles || []).some(r=>r.db==='admin' && r.role==='root')}}));
`

const mongoCreateAdminScript = `
  if (checked(admin.runCommand({usersInfo:{forAllDBs:true}})).users.length) throw Object.assign(new Error('Existing users'),{code:'MONGO_ADMIN_EXISTS'});
  checked(admin.runCommand({createUser:input.request.username,pwd:input.request.password,roles:[{role:'root',db:'admin'}]}));
  print(JSON.stringify({result:true}));
`

const mongoChangePasswordScript = `
  checked(connection.getDB(input.credentials.authDatabase).runCommand({updateUser:input.credentials.username,pwd:input.request.password}));
  print(JSON.stringify({result:true}));
`

const mongoResetPasswordScript = `
  checked(connection.getDB(input.request.authDatabase).runCommand({updateUser:input.request.username,pwd:input.request.password}));
  print(JSON.stringify({result:true}));
`
