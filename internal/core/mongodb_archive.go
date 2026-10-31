package core

import (
	"bufio"
	"bytes"
	"compress/gzip"
	"context"
	"encoding/binary"
	"hash/crc64"
	"io"
	"sort"
	"strconv"
	"strings"
	"unicode/utf8"
)

type mongoArchiveDatabase struct {
	Name        string `json:"name"`
	Collections int    `json:"collections"`
}
type mongoArchiveInfo struct {
	Version      string                 `json:"version"`
	ToolsVersion string                 `json:"toolsVersion"`
	Compression  string                 `json:"compression"`
	Databases    []mongoArchiveDatabase `json:"databases"`
}

// MongoDB archive v0.1 is a stream of bounded BSON frames, not a tar/zip.
// Only the fixed metadata vocabulary is decoded; document bytes remain opaque.
func mongoFrame(r io.Reader) (data []byte, terminator bool, err error) {
	var prefix [4]byte
	if _, err = io.ReadFull(r, prefix[:]); err != nil {
		return nil, false, err
	}
	n := binary.LittleEndian.Uint32(prefix[:])
	if n == 0xffffffff {
		return nil, true, nil
	}
	if n < 5 || n > 16*1024*1024 {
		return nil, false, invalid("MongoDB 归档 BSON 长度无效或超过 16 MiB")
	}
	data = make([]byte, int(n))
	copy(data, prefix[:])
	if _, err = io.ReadFull(r, data[4:]); err != nil {
		return nil, false, err
	}
	if data[len(data)-1] != 0 {
		return nil, false, invalid("MongoDB 归档 BSON 结束标记无效")
	}
	return data, false, nil
}

func mongoFields(frame []byte) (map[string]any, error) {
	result := map[string]any{}
	data := frame[4 : len(frame)-1]
	take := func(n int) ([]byte, error) {
		if n < 0 || n > len(data) {
			return nil, invalid("MongoDB 归档元信息不完整")
		}
		v := data[:n]
		data = data[n:]
		return v, nil
	}
	for len(data) > 0 {
		if len(result) >= 32 {
			return nil, invalid("MongoDB 归档元信息字段过多")
		}
		kind := data[0]
		data = data[1:]
		end := bytes.IndexByte(data, 0)
		if end < 0 || end > 128 || !utf8.Valid(data[:end]) {
			return nil, invalid("MongoDB 归档字段名无效")
		}
		name := string(data[:end])
		data = data[end+1:]
		if _, exists := result[name]; exists {
			return nil, invalid("MongoDB 归档包含重复字段")
		}
		switch kind {
		case 2:
			b, err := take(4)
			if err != nil {
				return nil, err
			}
			n := binary.LittleEndian.Uint32(b)
			if n == 0 || uint64(n) > uint64(len(data)) {
				return nil, invalid("MongoDB 归档字符串长度无效")
			}
			b, err = take(int(n))
			if err != nil {
				return nil, err
			}
			if b[len(b)-1] != 0 || !utf8.Valid(b[:len(b)-1]) {
				return nil, invalid("MongoDB 归档字符串编码无效")
			}
			result[name] = string(b[:len(b)-1])
		case 8:
			b, err := take(1)
			if err != nil {
				return nil, err
			}
			if b[0] > 1 {
				return nil, invalid("MongoDB 归档布尔值无效")
			}
			result[name] = b[0] == 1
		case 16:
			b, err := take(4)
			if err != nil {
				return nil, err
			}
			result[name] = uint64(int64(int32(binary.LittleEndian.Uint32(b))))
		case 18:
			b, err := take(8)
			if err != nil {
				return nil, err
			}
			result[name] = binary.LittleEndian.Uint64(b)
		default:
			return nil, invalid("MongoDB 归档元信息包含不支持的字段类型")
		}
	}
	return result, nil
}

func mongoArchiveVersion(s string) ([3]uint64, error) {
	var v [3]uint64
	parts := strings.Split(strings.SplitN(s, "-", 2)[0], ".")
	if len(parts) != 3 || !mongoName(s, 128) {
		return v, invalid("MongoDB 归档版本无效")
	}
	for i, p := range parts {
		n, err := strconv.ParseUint(p, 10, 32)
		if err != nil {
			return v, invalid("MongoDB 归档版本无法识别")
		}
		v[i] = n
	}
	return v, nil
}

func inspectMongoArchive(ctx context.Context, input io.Reader, selected string, output io.Writer) (mongoArchiveInfo, error) {
	info := mongoArchiveInfo{Compression: "none", Databases: []mongoArchiveDatabase{}}
	buffer := bufio.NewReader(input)
	prefix, err := buffer.Peek(2)
	if err != nil {
		return info, err
	}
	var reader io.Reader = buffer
	if bytes.Equal(prefix, []byte{0x1f, 0x8b}) {
		z, err := gzip.NewReader(buffer)
		if err != nil {
			return info, err
		}
		defer z.Close()
		reader = z
		info.Compression = "gzip"
	}
	var magic [4]byte
	if _, err = io.ReadFull(reader, magic[:]); err != nil {
		return info, err
	}
	if binary.LittleEndian.Uint32(magic[:]) != 0x8199e26d {
		return info, invalid("请选择完整的 mongodump archive 文件")
	}
	header, end, err := mongoFrame(reader)
	if err != nil {
		return info, err
	}
	if end {
		return info, invalid("MongoDB 归档头缺失")
	}
	head, err := mongoFields(header)
	if err != nil {
		return info, err
	}
	if head["version"] != "0.1" {
		return info, invalid("不支持此 MongoDB archive 格式版本")
	}
	info.Version, _ = head["server_version"].(string)
	info.ToolsVersion, _ = head["tool_version"].(string)
	version, err := mongoArchiveVersion(info.Version)
	if err != nil {
		return info, err
	}
	if _, err = mongoArchiveVersion(info.ToolsVersion); err != nil {
		return info, err
	}
	concurrent, _ := head["concurrent_collections"].(uint64)
	if concurrent < 1 || concurrent > 1024 {
		return info, invalid("MongoDB 归档并发参数无效")
	}
	emit := func(b []byte) error { _, err := output.Write(b); return err }
	if selected != "" {
		if err = emit(magic[:]); err != nil {
			return info, err
		}
		if err = emit(header); err != nil {
			return info, err
		}
	}
	type namespace struct {
		crc    uint64
		closed bool
	}
	entries := map[[2]string]*namespace{}
	databases := map[string]int{}
	metadataBytes := 0
	terminator := []byte{255, 255, 255, 255}
	for {
		if err = ctx.Err(); err != nil {
			return info, err
		}
		b, end, err := mongoFrame(reader)
		if err != nil {
			return info, err
		}
		if end {
			break
		}
		metadataBytes += len(b)
		if metadataBytes > 64*1024*1024 || len(entries) >= 100000 {
			return info, invalid("MongoDB 归档集合元信息过大")
		}
		fields, err := mongoFields(b)
		if err != nil {
			return info, err
		}
		db, _ := fields["db"].(string)
		collection, _ := fields["collection"].(string)
		if !mongoName(db, 63) || strings.ContainsAny(db, ".\x00") || !mongoName(collection, 512) {
			return info, invalid("MongoDB 归档集合名无效")
		}
		physical := collection
		if fields["type"] == "timeseries" && (version[0] < 8 || version[0] == 8 && version[1] < 3) {
			physical = "system.buckets." + collection
		}
		key := [2]string{db, physical}
		if entries[key] != nil {
			return info, invalid("MongoDB 归档包含重复集合")
		}
		entries[key] = &namespace{}
		databases[db]++
		if selected == db {
			if err = emit(b); err != nil {
				return info, err
			}
		}
	}
	if len(databases) == 0 || selected != "" && databases[selected] == 0 {
		return info, invalid("MongoDB 归档没有所选数据库或集合")
	}
	if selected != "" {
		if err = emit(terminator); err != nil {
			return info, err
		}
	}
	table := crc64.MakeTable(crc64.ECMA)
	for {
		if err = ctx.Err(); err != nil {
			return info, err
		}
		b, end, err := mongoFrame(reader)
		if err == io.EOF {
			break
		}
		if err != nil {
			return info, err
		}
		if end {
			return info, invalid("MongoDB 归档集合段缺少头部")
		}
		fields, err := mongoFields(b)
		if err != nil {
			return info, err
		}
		db, _ := fields["db"].(string)
		collection, _ := fields["collection"].(string)
		eof, ok := fields["EOF"].(bool)
		if !ok {
			return info, invalid("MongoDB 归档缺少 EOF 字段")
		}
		expected, ok := fields["CRC"].(uint64)
		if !ok {
			return info, invalid("MongoDB 归档缺少 CRC 字段")
		}
		entry := entries[[2]string{db, collection}]
		if entry == nil || entry.closed {
			return info, invalid("MongoDB 归档数据属于未声明或已结束的集合")
		}
		if selected == db {
			if err = emit(b); err != nil {
				return info, err
			}
		}
		for {
			if err = ctx.Err(); err != nil {
				return info, err
			}
			doc, end, err := mongoFrame(reader)
			if err != nil {
				return info, err
			}
			if end {
				if selected == db {
					if err = emit(terminator); err != nil {
						return info, err
					}
				}
				break
			}
			if eof {
				return info, invalid("MongoDB 归档 EOF 后仍有数据")
			}
			entry.crc = crc64.Update(entry.crc, table, doc)
			if selected == db {
				if err = emit(doc); err != nil {
					return info, err
				}
			}
		}
		if eof {
			if expected != entry.crc {
				return info, invalid("MongoDB 归档 CRC 校验失败")
			}
			entry.closed = true
		} else if expected != 0 {
			return info, invalid("MongoDB 归档非 EOF 段包含校验值")
		}
	}
	for _, entry := range entries {
		if !entry.closed {
			return info, invalid("MongoDB 归档缺少集合结束标记")
		}
	}
	for name, count := range databases {
		info.Databases = append(info.Databases, mongoArchiveDatabase{name, count})
	}
	sort.Slice(info.Databases, func(i, j int) bool { return info.Databases[i].Name < info.Databases[j].Name })
	return info, nil
}
