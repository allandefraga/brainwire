"""Deterministic Python syntax extraction with source evidence; never executes code."""
import ast
import hashlib
import json
import os
import stat
from pathlib import PurePosixPath

_PROPERTIES = ['namespace', 'kind', 'name', 'qualified_name', 'source_path',
               'source_hash', 'start_line', 'end_line', 'evidence', 'alias', 'generation_id',
               'start_column', 'end_column', 'call_ordinal']
_EVIDENCE = ['source_path', 'source_hash', 'start_line', 'end_line', 'evidence']
_RELATIONS = ('DEFINES', 'IMPORTS', 'LOCAL_MODULE_CANDIDATE', 'HAS_CALL', 'STATIC_CALL_CANDIDATE')


def _graph_json_size(value, limit, message):
    # Generated graph records have bounded fields; encode small slices instead
    # of allocating a second complete graph just to count its bytes.
    size = 2
    for index, (key, item) in enumerate(value.items()):
        size += (2 if index else 0) + len(json.dumps(key, ensure_ascii=False).encode('utf-8')) + 2
        if size > limit:
            raise ValueError(message)
        if isinstance(item, list):
            size += 2
            for start in range(0, len(item), 32):
                part = json.dumps(item[start:start + 32], ensure_ascii=False).encode('utf-8')
                size += len(part) - 2 + (2 if start else 0)
                if size > limit:
                    raise ValueError(message)
        else:
            size += len(json.dumps(item, ensure_ascii=False).encode('utf-8'))
        if size > limit:
            raise ValueError(message)
    if size > limit:
        raise ValueError(message)
    return size


def ontology():
    """Return the ontology to declare before loading extracted code snapshots."""
    return {'version': '1', 'nodes': [{'label': 'CodeEntity', 'properties': list(_PROPERTIES),
            'indexed_fields': ['kind', 'source_path', 'source_hash', 'name']}],
            'edges': [{'type': relation, 'from': {'label': 'CodeEntity'},
                       'to': {'label': 'CodeEntity'}, 'properties': [*_EVIDENCE, 'generation_id', 'resolution']}
                      for relation in _RELATIONS]}


def _text(value, name, maximum):
    if not isinstance(value, str) or not value or len(value) > maximum or len(value.encode('utf-8')) > maximum \
            or any(ord(char) < 32 or 127 <= ord(char) < 160 for char in value):
        raise ValueError(f'{name} must be nonempty bounded text without control characters')
    return value


def extract_python(source, *, source_path, namespace, max_entities=10000):
    """Extract source syntax without execution; IDs identify the immutable file snapshot."""
    artifact, _ = _extract_python(source, source_path=source_path, namespace=namespace,
                                  max_entities=max_entities)
    return artifact


def _extract_python(source, *, source_path, namespace, max_entities=10000):
    """Extract definitions and import occurrences from at most 1 MiB of Python.

    source_path is a logical relative POSIX path, never opened by this function.
    IDs identify a content snapshot. Changed content produces new identities;
    this function does not reconcile or delete prior snapshots.
    """
    source_path = _text(source_path, 'source_path', 1024)
    path = PurePosixPath(source_path)
    if path.is_absolute() or '..' in path.parts or '\\' in source_path \
            or str(path) != source_path or source_path == '.':
        raise ValueError('source_path must be a normalized relative POSIX path')
    namespace = _text(namespace, 'namespace', 256)
    if not isinstance(source, str) or len(source) > 1048576 or len(source.encode('utf-8')) > 1048576:
        raise ValueError('source must be Python text of at most 1 MiB UTF-8')
    if type(max_entities) is not int or not 1 <= max_entities <= 10000:
        raise ValueError('max_entities must be an integer from 1 to 10000')
    tree = ast.parse(source, filename=source_path)
    if sum(1 for _ in ast.walk(tree)) > 100000:
        raise ValueError('source exceeds 100000 AST nodes')
    source_hash = hashlib.sha256(source.encode('utf-8')).hexdigest()
    snapshot = json.dumps([namespace, source_path, source_hash], ensure_ascii=False)
    nodes, edges = [], []

    def add(kind, name, qualified, start, end, alias=None):
        if len(nodes) >= max_entities:
            raise ValueError('source exceeds max_entities')
        _text(name, 'entity name', 4096)
        _text(qualified, 'qualified name', 4096)
        if alias is not None:
            _text(alias, 'alias', 4096)
        identity = json.dumps([snapshot, kind, qualified, start, len(nodes)], ensure_ascii=False)
        node = {'entity_id': 'code:' + hashlib.sha256(identity.encode('utf-8')).hexdigest(),
                'namespace': namespace, 'kind': kind, 'name': name, 'qualified_name': qualified,
                'source_path': source_path, 'source_hash': source_hash,
                'start_line': start, 'end_line': end, 'evidence': 'EXTRACTED', 'alias': alias}
        nodes.append(node)
        return node

    file = add('file', source_path, source_path, 1, max(1, source.count('\n') + 1))

    def link(owner, node, relation):
        edges.append({'source_id': owner['entity_id'], 'target_id': node['entity_id'],
                      'relationship': relation, **{key: node[key] for key in _EVIDENCE}})

    class Extractor(ast.NodeVisitor):
        def __init__(self):
            self.owner = file
            self.names = []

        def definition(self, node, kind):
            qualified = '.'.join([*self.names, node.name])
            entity = add(kind, node.name, qualified, node.lineno, node.end_lineno)
            link(self.owner, entity, 'DEFINES')
            previous = self.owner
            self.owner = entity
            self.names.append(node.name)
            self.generic_visit(node)
            self.names.pop()
            self.owner = previous

        def visit_ClassDef(self, node): self.definition(node, 'class')
        def visit_FunctionDef(self, node): self.definition(node, 'function')
        def visit_AsyncFunctionDef(self, node): self.definition(node, 'function')

        def imports(self, node, prefix):
            for item in node.names:
                name = prefix + item.name
                entity = add('import', name, '.'.join([*self.names, name]),
                             node.lineno, node.end_lineno, item.asname)
                link(self.owner, entity, 'IMPORTS')

        def visit_Import(self, node): self.imports(node, '')
        def visit_ImportFrom(self, node):
            self.imports(node, '.' * node.level + (node.module or '') + ':')

    Extractor().visit(tree)
    artifact = {'schema_version': '1', 'extractor': 'brainwire-python-ast-v1',
                'source_hash': source_hash, 'nodes': nodes, 'edges': edges}
    _graph_json_size(artifact, 8 << 20, 'extracted graph exceeds 8 MiB')
    return artifact, tree


def load_python(graph, source, *, source_path, namespace, max_entities=10000):
    """Load one extracted snapshot through bounded GraphClient upsert batches.

    Declare ontology() first. Nodes commit before edges; each batch is separately
    atomic. Retry identical immutable input only after reconciling uncertain
    commits. This does not configure ACL, infer relations or delete old snapshots.
    """
    artifact = extract_python(source, source_path=source_path, namespace=namespace,
                              max_entities=max_entities)
    return _load_artifact(graph, artifact)


def _load_artifact(graph, artifact):
    nodes = graph.load_nodes('CodeEntity', artifact['nodes'])
    edges = 0
    for relation in _RELATIONS:
        rows = [{key: value for key, value in edge.items() if key != 'relationship'}
                for edge in artifact['edges'] if edge['relationship'] == relation]
        if rows:
            edges += graph.load_edges('CodeEntity', relation, 'CodeEntity', rows,
                                      require_endpoints=True)
    return {'nodes': nodes, 'edges': edges, 'source_hash': artifact['source_hash']}


# Descriptor-relative traversal must remain available even when tests wrap os.open.
_DESCRIPTOR_TRAVERSAL = (os.name == 'posix' and hasattr(os, 'O_NOFOLLOW')
                         and hasattr(os, 'O_DIRECTORY')
                         and os.open in os.supports_dir_fd
                         and os.stat in os.supports_dir_fd
                         and os.scandir in os.supports_fd)
_EXCLUDED_DIRECTORIES = frozenset({'.git', '.venv', 'venv', '__pycache__',
                                  'node_modules', 'target', 'build', 'dist'})


def extract_python_directory(root, *, namespace, max_files=100, max_entities=10000,
                             resolve_imports=False, resolve_calls=False):
    """Read a bounded local Python tree using POSIX no-follow directory handles.

    Reject symlinks outside excluded entries; never execute source. All files are
    extracted before callers may write anything. Filesystem reads are not an
    atomic snapshot: use an immutable checkout for reproducible ingestion.
    """
    _text(namespace, 'namespace', 256)
    if type(resolve_imports) is not bool or type(resolve_calls) is not bool:
        raise ValueError('resolve_imports/resolve_calls must be booleans')
    if type(max_files) is not int or not 1 <= max_files <= 100:
        raise ValueError('max_files must be an integer from 1 to 100')
    if type(max_entities) is not int or not 1 <= max_entities <= 10000:
        raise ValueError('max_entities must be an integer from 1 to 10000')
    if not _DESCRIPTOR_TRAVERSAL:
        raise ValueError('safe directory extraction requires POSIX descriptor traversal')
    nodes, edges, files = [], [], []
    facts = {}
    entry_count = source_bytes = artifact_bytes = 0
    directory_flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW

    def visit(fd, prefix, depth):
        nonlocal entry_count, source_bytes, artifact_bytes
        names = []
        with os.scandir(fd) as entries:
            for entry in entries:
                entry_count += 1
                if entry_count > 10000:
                    raise ValueError('source tree exceeds 10000 directory entries')
                names.append(entry.name)
        for name in sorted(names):
            if name in _EXCLUDED_DIRECTORIES:
                continue
            metadata = os.stat(name, dir_fd=fd, follow_symlinks=False)
            if stat.S_ISLNK(metadata.st_mode):
                raise ValueError('source tree contains a symlink')
            path = '/'.join([*prefix, name])
            if stat.S_ISDIR(metadata.st_mode):
                if depth >= 16:
                    raise ValueError('source tree exceeds 16 directory levels')
                child = os.open(name, directory_flags, dir_fd=fd)
                try:
                    visit(child, [*prefix, name], depth + 1)
                finally:
                    os.close(child)
            elif name.endswith('.py'):
                if not stat.S_ISREG(metadata.st_mode):
                    raise ValueError('Python source must be a regular file')
                if len(files) >= max_files:
                    raise ValueError('source tree exceeds max_files')
                source_fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK,
                                    dir_fd=fd)
                with os.fdopen(source_fd, 'rb') as stream:
                    if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
                        raise ValueError('Python source must be a regular file')
                    data = stream.read(1048577)
                source_bytes += len(data)
                if len(data) > 1048576 or source_bytes > 8 << 20:
                    raise ValueError('source tree exceeds per-file or aggregate source bounds')
                remaining = max_entities - len(nodes)
                if remaining < 1:
                    raise ValueError('source tree exceeds max_entities')
                artifact, tree = _extract_python(data.decode('utf-8'), source_path=path,
                                                 namespace=namespace, max_entities=remaining)
                if resolve_calls:
                    facts[path] = _static_call_facts(tree, artifact)
                    if sum(len(item['calls']) for item in facts.values()) > 10000:
                        raise ValueError('source tree exceeds static call analysis bounds')
                artifact_bytes += _graph_json_size(artifact, (8 << 20) - artifact_bytes,
                                                   'source tree graph exceeds 8 MiB')
                files.append({'path': path, 'source_hash': artifact['source_hash']})
                nodes.extend(artifact['nodes'])
                edges.extend(artifact['edges'])

    try:
        root_fd = os.open(os.fspath(root), directory_flags)
        try:
            visit(root_fd, [], 0)
        finally:
            os.close(root_fd)
    except OSError as error:
        raise ValueError('cannot safely read source tree') from error
    manifest = json.dumps(files, ensure_ascii=False, separators=(',', ':')).encode('utf-8')
    result = {'schema_version': '1', 'extractor': 'brainwire-python-directory-v1',
              'source_hash': hashlib.sha256(manifest).hexdigest(),
              'files': files, 'nodes': nodes, 'edges': edges}
    if resolve_imports or resolve_calls:
        identities = _local_import_candidates(result, namespace, calls=resolve_calls)
        if resolve_calls:
            _static_call_candidates(result, facts, identities, max_entities)
    _graph_json_size(result, 8 << 20, 'source tree graph exceeds 8 MiB')
    return result


def load_python_directory(graph, root, *, namespace, max_files=100, max_entities=10000,
                          resolve_imports=False, resolve_calls=False):
    """Extract the whole bounded tree, then upsert batches without deleting history."""
    artifact = extract_python_directory(root, namespace=namespace, max_files=max_files,
                                        max_entities=max_entities, resolve_imports=resolve_imports,
                                        resolve_calls=resolve_calls)
    receipt = _load_artifact(graph, artifact)
    if 'generation_id' in artifact:
        receipt['generation_id'] = artifact['generation_id']
    return {**receipt, 'files': len(artifact['files'])}


def _local_import_candidates(artifact, namespace, calls=False):
    """Add explicitly inferred local path candidates inside one directory generation."""
    import keyword

    mode = 'directory-static-call-v1' if calls else 'directory-static-import-v1'
    manifest = json.dumps([mode, namespace, artifact['files']],
                          ensure_ascii=False, separators=(',', ':')).encode('utf-8')
    generation = hashlib.sha256(manifest).hexdigest()
    identities = {node['entity_id']: 'code:' + hashlib.sha256(
        json.dumps([generation, node['entity_id']], separators=(',', ':')).encode('utf-8')
    ).hexdigest() for node in artifact['nodes']}
    artifact['nodes'] = [{**node, 'entity_id': identities[node['entity_id']],
                          'generation_id': generation} for node in artifact['nodes']]
    artifact['edges'] = [{**edge, 'source_id': identities[edge['source_id']],
                          'target_id': identities[edge['target_id']],
                          'generation_id': generation} for edge in artifact['edges']]
    files = {node['source_path']: node for node in artifact['nodes'] if node['kind'] == 'file'}
    modules = {}
    for path in files:
        parts = list(PurePosixPath(path).parts)
        parts[-1] = parts[-1][:-3]
        if parts[-1] == '__init__':
            parts.pop()
        if parts and all(part.isidentifier() and not keyword.iskeyword(part) for part in parts):
            modules.setdefault('.'.join(parts), []).append(path)

    def package(parts):
        return modules.get('.'.join(parts)) == ['/'.join([*parts, '__init__.py'])]

    def target(parts):
        paths = modules.get('.'.join(parts), [])
        if len(paths) != 1 or not all(package(parts[:index]) for index in range(1, len(parts))):
            return None
        return files[paths[0]]

    for node in artifact['nodes']:
        if node['kind'] != 'import':
            continue
        spelling = node['name'].split(':', 1)[0]
        level = len(spelling) - len(spelling.lstrip('.'))
        module = spelling[level:]
        parts = module.split('.') if module else []
        if level:
            current_package = list(PurePosixPath(node['source_path']).parts[:-1])
            if level > len(current_package) or not all(
                    package(current_package[:index]) for index in range(1, len(current_package) + 1)):
                continue
            parts = current_package[:len(current_package) - level + 1] + parts
        destination = target(parts)
        if destination is not None:
            artifact['edges'].append({
                'source_id': node['entity_id'], 'target_id': destination['entity_id'],
                'relationship': 'LOCAL_MODULE_CANDIDATE',
                **{key: node[key] for key in _EVIDENCE},
                'evidence': 'INFERRED', 'resolution': 'STATIC_LOCAL_PATH',
                'generation_id': generation,
            })
    artifact['generation_id'] = generation
    artifact['extractor'] = 'brainwire-python-directory-imports-v1'
    return identities


def _static_call_facts(tree, artifact):
    """Conservative bindings and direct calls from the same AST used for source hashing."""
    from collections import Counter

    bindings = Counter()
    attribute_mutations = set()
    unsafe = False
    reflection = {'exec', 'eval', 'globals', 'locals', 'setattr', 'delattr', '__import__'}
    for node in ast.walk(tree):
        if isinstance(node, ast.Name) and isinstance(node.ctx, (ast.Store, ast.Del)):
            bindings[node.id] += 1
        elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            bindings[node.name] += 1
        elif isinstance(node, ast.arg):
            bindings[node.arg] += 1
        elif isinstance(node, (ast.Global, ast.Nonlocal)):
            bindings.update(node.names)
        elif isinstance(node, ast.ExceptHandler) and node.name:
            bindings[node.name] += 1
        elif isinstance(node, (ast.Import, ast.ImportFrom)):
            for item in node.names:
                if item.name == '*':
                    unsafe = True
                bindings[item.asname or (item.name.split('.')[0] if isinstance(node, ast.Import) else item.name)] += 1
        elif type(node).__name__ in ('MatchAs', 'MatchStar', 'TypeVar', 'ParamSpec', 'TypeVarTuple'):
            name = getattr(node, 'name', None)
            if isinstance(name, str):
                bindings[name] += 1
        elif type(node).__name__ == 'MatchMapping':
            name = getattr(node, 'rest', None)
            if name:
                bindings[name] += 1
        if isinstance(node, (ast.Attribute, ast.Subscript)) and isinstance(node.ctx, (ast.Store, ast.Del)):
            base = node.value
            while isinstance(base, (ast.Attribute, ast.Subscript)):
                base = base.value
            if isinstance(base, ast.Name):
                attribute_mutations.add(base.id)
        if isinstance(node, ast.Call) and (
                isinstance(node.func, ast.Name) and node.func.id in reflection
                or isinstance(node.func, ast.Attribute) and node.func.attr in reflection):
            unsafe = True

    # Aliases/functions used as values can escape and be mutated through another name.
    parents = {id(child): parent for parent in ast.walk(tree) for child in ast.iter_child_nodes(parent)}
    escaped = set()
    for node in ast.walk(tree):
        if not isinstance(node, ast.Name) or not isinstance(node.ctx, ast.Load):
            continue
        parent = parents.get(id(node))
        grandparent = parents.get(id(parent))
        direct = isinstance(parent, ast.Call) and parent.func is node
        module_call = isinstance(parent, ast.Attribute) and parent.value is node \
            and isinstance(grandparent, ast.Call) and grandparent.func is parent
        if not direct and not module_call:
            escaped.add(node.id)

    entities = artifact['nodes']
    definitions = {(item['qualified_name'], item['start_line']): item for item in entities if item['kind'] == 'function'}
    occurrences = {(item['name'], item['start_line'], item['alias']): item for item in entities if item['kind'] == 'import'}
    functions = {}
    top_functions = []
    for node in tree.body:
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) \
                and bindings[node.name] == 1 and node.name not in attribute_mutations \
                and node.name not in escaped and not node.decorator_list:
            entity = definitions[node.name, node.lineno]
            functions[node.name] = entity['entity_id']
            top_functions.append((node, entity))
    imports = {}
    for node in tree.body:
        if not isinstance(node, (ast.Import, ast.ImportFrom)):
            continue
        for item in node.names:
            alias = item.asname or (item.name.split('.')[0] if isinstance(node, ast.Import) else item.name)
            if bindings[alias] != 1 or alias in attribute_mutations or alias in escaped or item.name == '*':
                continue
            if isinstance(node, ast.Import) and '.' in item.name and not item.asname:
                continue
            spelling = item.name if isinstance(node, ast.Import) else \
                '.' * node.level + (node.module or '') + ':' + item.name
            # Unique bindings make occurrence lookup unambiguous even for multi-import statements.
            entity = occurrences[spelling, node.lineno, item.asname]
            imports[alias] = {'occurrence': entity['entity_id'],
                              'member': None if isinstance(node, ast.Import) else item.name}
    calls = []

    class Calls(ast.NodeVisitor):
        def __init__(self, owner): self.owner = owner
        def visit_FunctionDef(self, node): pass
        def visit_AsyncFunctionDef(self, node): pass
        def visit_ClassDef(self, node): pass
        def visit_Lambda(self, node): pass
        def visit_Call(self, node):
            alias, member = None, None
            if isinstance(node.func, ast.Name):
                alias = node.func.id
            elif isinstance(node.func, ast.Attribute) and isinstance(node.func.value, ast.Name):
                alias, member = node.func.value.id, node.func.attr
            if alias is not None and len(alias) <= 4096 and (member is None or len(member) <= 4096):
                if len(calls) >= 10000:
                    raise ValueError('source exceeds static call analysis bounds')
                calls.append({'owner': self.owner['entity_id'], 'alias': alias, 'member': member,
                              'start_line': node.lineno, 'end_line': node.end_lineno,
                              'start_column': node.col_offset, 'end_column': node.end_col_offset,
                              'ordinal': len(calls)})
            self.generic_visit(node)

    if not unsafe:
        for node, entity in top_functions:
            visitor = Calls(entity)
            for statement in node.body:
                visitor.visit(statement)
    return {'unsafe': unsafe, 'functions': functions, 'imports': imports, 'calls': calls}


def _static_call_candidates(artifact, facts, identities, max_entities):
    generation = artifact['generation_id']
    nodes = {node['entity_id']: node for node in artifact['nodes']}
    module_paths = {edge['source_id']: nodes[edge['target_id']]['source_path']
                    for edge in artifact['edges'] if edge['relationship'] == 'LOCAL_MODULE_CANDIDATE'}
    for source_path, source in facts.items():
        if source['unsafe']:
            continue
        for call in source['calls']:
            imported = source['imports'].get(call['alias'])
            if imported is None:
                continue
            module = module_paths.get(identities[imported['occurrence']])
            if module is None or facts[module]['unsafe']:
                continue
            member = imported['member']
            if member is None:
                member = call['member']
            elif call['member'] is not None:
                continue
            target = facts[module]['functions'].get(member)
            if target is None:
                continue
            if len(artifact['nodes']) >= max_entities:
                raise ValueError('source tree exceeds max_entities including call sites')
            owner = nodes[identities[call['owner']]]
            spelling = call['alias'] + ('.' + call['member'] if call['member'] is not None else '') + '()'
            qualified = owner['qualified_name'] + f".<call@{call['start_line']}:{call['start_column']}:{call['ordinal']}>"
            _text(spelling, 'call spelling', 4096)
            _text(qualified, 'call qualified name', 4096)
            identity = 'code:' + hashlib.sha256(json.dumps(
                [generation, 'call', source_path, call['start_line'], call['start_column'],
                 call['end_line'], call['end_column'], call['ordinal']], separators=(',', ':')).encode('utf-8')).hexdigest()
            entity = {'entity_id': identity, 'namespace': owner['namespace'], 'kind': 'call',
                      'name': spelling, 'qualified_name': qualified, 'source_path': source_path,
                      'source_hash': owner['source_hash'], 'evidence': 'EXTRACTED', 'alias': None,
                      'generation_id': generation, 'start_line': call['start_line'],
                      'end_line': call['end_line'], 'start_column': call['start_column'],
                      'end_column': call['end_column'], 'call_ordinal': call['ordinal']}
            artifact['nodes'].append(entity)
            evidence = {key: entity[key] for key in _EVIDENCE}
            artifact['edges'].append({'source_id': owner['entity_id'], 'target_id': identity,
                'relationship': 'HAS_CALL', **evidence, 'generation_id': generation})
            artifact['edges'].append({'source_id': identity, 'target_id': identities[target],
                'relationship': 'STATIC_CALL_CANDIDATE', **evidence, 'evidence': 'INFERRED',
                'resolution': 'STATIC_IMPORT_BINDING', 'generation_id': generation})
    artifact['extractor'] = 'brainwire-python-directory-calls-v1'


def degree_summary(artifact, *, limit=10):
    """Rank bounded extracted entities by distinct directed syntax-edge degree.

    Equal endpoint/type edges count once. Different types count separately, and
    a self-loop contributes one inbound and one outbound edge. Degree is not
    semantic relevance or a calibrated importance score.
    """
    if type(limit) is not int or not 1 <= limit <= 20:
        raise ValueError('degree limit must be an integer from 1 to 20')
    if not isinstance(artifact, dict) or artifact.get('schema_version') != '1':
        raise ValueError('unsupported code graph artifact')
    nodes, edges = artifact.get('nodes'), artifact.get('edges')
    if not isinstance(nodes, list) or not isinstance(edges, list) \
            or len(nodes) > 10000 or len(edges) > 20000:
        raise ValueError('code graph degree input exceeds extraction bounds')
    entities, counts = {}, {}
    for node in nodes:
        if not isinstance(node, dict):
            raise ValueError('code graph entities must be maps')
        identity = _text(node.get('entity_id'), 'entity ID', 256)
        if identity in entities:
            raise ValueError('code graph entity IDs must be unique')
        _text(node.get('name'), 'entity name', 4096)
        _text(node.get('source_path'), 'source_path', 1024)
        if node.get('kind') not in ('file', 'class', 'function', 'import', 'call'):
            raise ValueError('unsupported code graph entity kind')
        entities[identity] = node
        counts[identity] = [0, 0]
    seen = set()
    for edge in edges:
        if not isinstance(edge, dict):
            raise ValueError('code graph relationships must be maps')
        source = _text(edge.get('source_id'), 'source ID', 256)
        target = _text(edge.get('target_id'), 'target ID', 256)
        relation = edge.get('relationship')
        if source not in entities or target not in entities \
                or relation not in _RELATIONS:
            raise ValueError('invalid code graph relationship')
        key = source, relation, target
        if key not in seen:
            seen.add(key)
            counts[source][1] += 1
            counts[target][0] += 1
    ordered = sorted(entities, key=lambda identity: (-sum(counts[identity]), identity))
    def preview(value, maximum):
        return value if len(value) <= maximum else value[:maximum] + '\u2026'
    return [{'entity_id': identity, 'name': preview(entities[identity]['name'], 120),
             'kind': entities[identity]['kind'],
             'source_path': preview(entities[identity]['source_path'], 200),
             'in_degree': counts[identity][0], 'out_degree': counts[identity][1],
             'degree': sum(counts[identity])}
            for identity in ordered[:limit]]


def main(argv=None):
    """Map a local source tree without a running database or model."""
    import argparse
    import sys

    parser = argparse.ArgumentParser(
        description='Map Python definitions/imports locally, with source evidence. No code execution.')
    parser.add_argument('root', help='approved Python source directory (POSIX)')
    parser.add_argument('--namespace', required=True, help='explicit source namespace')
    parser.add_argument('--resolve-calls', action='store_true',
                        help='infer strictly limited imported-function call candidates')
    parser.add_argument('--resolve-imports', action='store_true',
                        help='infer local module path candidates within a complete generation')
    parser.add_argument('--max-files', type=int, default=100, help='file bound from 1 to 100')
    parser.add_argument('--max-entities', type=int, default=10000,
                        help='entity bound from 1 to 10000')
    output_mode = parser.add_mutually_exclusive_group()
    output_mode.add_argument('--top-degree', type=int,
                             help='include the 1-20 most connected syntax entities')
    output_mode.add_argument('--json', action='store_true',
                        help='emit the complete bounded artifact instead of a small summary')
    args = parser.parse_args(argv)
    if args.top_degree is not None and not 1 <= args.top_degree <= 20:
        parser.error('--top-degree must be an integer from 1 to 20')
    try:
        artifact = extract_python_directory(
            args.root, namespace=args.namespace, max_files=args.max_files,
            max_entities=args.max_entities, resolve_imports=args.resolve_imports,
            resolve_calls=args.resolve_calls)
    except SyntaxError:
        print('error: invalid Python syntax in source tree', file=sys.stderr)
        return 2
    except UnicodeError:
        print('error: source tree must contain UTF-8 Python text', file=sys.stderr)
        return 2
    except ValueError as error:
        print(f'error: {error}', file=sys.stderr)
        return 2
    except RecursionError:
        print('error: source nesting exceeds Python parser/extractor limits', file=sys.stderr)
        return 2
    output = artifact if args.json else {
        'schema_version': artifact['schema_version'],
        'extractor': artifact['extractor'], 'namespace': args.namespace,
        'files': len(artifact['files']), 'entities': len(artifact['nodes']),
        'relationships': len(artifact['edges']),
        'evidence': ('EXTRACTED_AND_INFERRED' if any(
            edge['evidence'] == 'INFERRED' for edge in artifact['edges']) else 'EXTRACTED'),
        'source_hash': artifact['source_hash'],
    }
    if (args.resolve_imports or args.resolve_calls) and not args.json:
        output['generation_id'] = artifact['generation_id']
    if args.top_degree is not None:
        output['top_degree'] = degree_summary(artifact, limit=args.top_degree)
    print(json.dumps(output, ensure_ascii=False, separators=(',', ':')))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
