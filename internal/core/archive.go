package core

import (
	"archive/tar"
	"archive/zip"
	"compress/gzip"
	"context"
	"errors"
	"io"
	"os"
	"path"
	"path/filepath"
	"runtime"
	"strings"

	"github.com/bodgit/sevenzip"
)

const maxExtractBytes int64 = 16 * 1024 * 1024 * 1024
const maxArchiveFiles = 300000

type contextReader struct {
	ctx    context.Context
	reader io.Reader
}

func (r contextReader) Read(data []byte) (int, error) {
	if err := r.ctx.Err(); err != nil {
		return 0, err
	}
	return r.reader.Read(data)
}

func archivePath(root, name string) (string, error) {
	name = strings.ReplaceAll(name, `\`, "/")
	if strings.HasPrefix(name, "/") || strings.ContainsAny(name, ":\x00") {
		return "", invalid("压缩包含有绝对路径或特殊文件名")
	}
	for _, part := range strings.Split(name, "/") {
		if part == ".." {
			return "", invalid("压缩包含有越界路径")
		}
		if part == "" || part == "." {
			continue
		}
		if runtime.GOOS == "windows" {
			base := strings.ToUpper(strings.SplitN(part, ".", 2)[0])
			if strings.ContainsAny(part, `<>"|?*`) || strings.HasSuffix(part, ".") || strings.HasSuffix(part, " ") || base == "CON" || base == "PRN" || base == "AUX" || base == "NUL" || len(base) == 4 && (strings.HasPrefix(base, "COM") || strings.HasPrefix(base, "LPT")) && base[3] >= '1' && base[3] <= '9' {
				return "", invalid("压缩包包含 Windows 不支持的文件名")
			}
		}
	}
	clean := path.Clean(name)
	if clean == "." {
		return root, nil
	}
	return relativePath(root, clean)
}

type archiveWriter struct {
	ctx   context.Context
	root  string
	bytes int64
	files int
}

func (w *archiveWriter) write(name string, size int64, mode os.FileMode, reader io.Reader) error {
	if err := w.ctx.Err(); err != nil {
		return err
	}
	w.files++
	if w.files > maxArchiveFiles || size < 0 || size > maxExtractBytes-w.bytes {
		return &AppError{Code: "ARCHIVE_LIMIT", Message: "压缩包展开体积或文件数量超过限制"}
	}
	target, err := archivePath(w.root, name)
	if err != nil {
		return err
	}
	if mode.IsDir() {
		return os.MkdirAll(target, 0755)
	}
	if !mode.IsRegular() {
		return &AppError{Code: "ARCHIVE_SPECIAL_FILE", Message: "压缩包包含链接或特殊文件，未安装"}
	}
	if target == w.root {
		return invalid("压缩包文件名为空")
	}
	if err = os.MkdirAll(filepath.Dir(target), 0755); err != nil {
		return err
	}
	permissions := os.FileMode(0644)
	if mode&0111 != 0 {
		permissions = 0755
	}
	file, err := os.OpenFile(target, os.O_CREATE|os.O_EXCL|os.O_WRONLY, permissions)
	if err != nil {
		return err
	}
	written, copyErr := io.Copy(file, io.LimitReader(contextReader{w.ctx, reader}, size+1))
	closeErr := file.Close()
	if copyErr != nil || closeErr != nil {
		return errors.Join(copyErr, closeErr)
	}
	if written != size {
		return &AppError{Code: "ARCHIVE_SIZE", Message: "压缩包文件长度与清单不一致"}
	}
	w.bytes += written
	return nil
}
func extractPackage(ctx context.Context, archive, destination, kind, entry, sourceURL string) error {
	w := archiveWriter{ctx: ctx, root: destination}
	switch kind {
	case "sevenzip":
		reader, err := sevenzip.OpenReader(archive)
		if err != nil {
			return err
		}
		defer reader.Close()
		if len(reader.File) > maxArchiveFiles {
			return &AppError{Code: "ARCHIVE_LIMIT", Message: "压缩包文件数量超过限制"}
		}
		for _, file := range reader.File {
			if file.UncompressedSize > uint64(maxExtractBytes) {
				return &AppError{Code: "ARCHIVE_LIMIT", Message: "压缩包文件过大"}
			}
			input, err := file.Open()
			if err != nil {
				return err
			}
			err = w.write(file.Name, int64(file.UncompressedSize), file.Mode(), input)
			closeErr := input.Close()
			if err != nil || closeErr != nil {
				return errors.Join(err, closeErr)
			}
		}
		return nil
	case "archive":
		reader, err := zip.OpenReader(archive)
		if err != nil {
			return err
		}
		defer reader.Close()
		for _, file := range reader.File {
			if file.UncompressedSize64 > uint64(maxExtractBytes) {
				return &AppError{Code: "ARCHIVE_LIMIT", Message: "压缩包文件过大"}
			}
			input, err := file.Open()
			if err != nil {
				return err
			}
			err = w.write(file.Name, int64(file.UncompressedSize64), file.Mode(), input)
			closeErr := input.Close()
			if err != nil || closeErr != nil {
				return errors.Join(err, closeErr)
			}
		}
		return nil
	case "targz":
		file, err := os.Open(archive)
		if err != nil {
			return err
		}
		defer file.Close()
		gzipReader, err := gzip.NewReader(contextReader{ctx, file})
		if err != nil {
			return err
		}
		defer gzipReader.Close()
		lower := strings.ToLower(strings.SplitN(sourceURL, "?", 2)[0])
		if strings.HasSuffix(lower, ".gz") && !strings.HasSuffix(lower, ".tar.gz") {
			target, err := archivePath(destination, entry)
			if err != nil {
				return err
			}
			if err = os.MkdirAll(filepath.Dir(target), 0755); err != nil {
				return err
			}
			output, err := os.OpenFile(target, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0755)
			if err != nil {
				return err
			}
			n, copyErr := io.Copy(output, io.LimitReader(contextReader{ctx, gzipReader}, maxExtractBytes+1))
			closeErr := output.Close()
			if n > maxExtractBytes {
				return &AppError{Code: "ARCHIVE_LIMIT", Message: "解压文件超过限制"}
			}
			return errors.Join(copyErr, closeErr)
		}
		reader := tar.NewReader(gzipReader)
		for {
			header, err := reader.Next()
			if errors.Is(err, io.EOF) {
				// Read through gzip's trailer so CRC/truncation errors cannot hide
				// behind tar's earlier end marker.
				remaining := maxExtractBytes - w.bytes
				n, drainErr := io.Copy(io.Discard, io.LimitReader(contextReader{ctx, gzipReader}, remaining+1))
				if n > remaining {
					return &AppError{Code: "ARCHIVE_LIMIT", Message: "压缩包尾部展开体积超过限制"}
				}
				err = drainErr
				return err
			}
			if err != nil {
				return err
			}
			if header.Typeflag != tar.TypeReg && header.Typeflag != tar.TypeRegA && header.Typeflag != tar.TypeDir {
				return &AppError{Code: "ARCHIVE_SPECIAL_FILE", Message: "压缩包包含链接或特殊文件，未安装"}
			}
			if err = w.write(header.Name, header.Size, header.FileInfo().Mode(), reader); err != nil {
				return err
			}
		}
	case "binary":
		file, err := os.Open(archive)
		if err != nil {
			return err
		}
		defer file.Close()
		info, err := file.Stat()
		if err != nil {
			return err
		}
		return w.write(entry, info.Size(), 0755, file)
	default:
		return &AppError{Code: "ARCHIVE_FORMAT_PENDING", Message: "此压缩格式的 Go 解压器尚在迁移", Detail: kind}
	}
}
