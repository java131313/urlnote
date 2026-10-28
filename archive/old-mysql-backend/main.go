package main

import (
	"database/sql"
	"fmt"
	"log"
	"net/http"
	"os"
	"regexp"
	"time"

	"github.com/gin-gonic/gin"
	_ "github.com/go-sql-driver/mysql"
)

var addr = ":3007" // PORT env overrides, e.g. PORT=3008

var idRe = regexp.MustCompile(`^[a-zA-Z0-9_-]{1,64}$`)

// ---- database ----

func dsn() string {
	if v := os.Getenv("MYSQL_DSN"); v != "" {
		return v
	}
	host := getenv("DB_HOST", "localhost")
	port := getenv("DB_PORT", "3306")
	user := getenv("DB_USER", "urlnote")
	pass := getenv("DB_PASSWORD", "urlnote")
	name := getenv("DB_NAME", "urlnote")
	return user + ":" + pass + "@tcp(" + host + ":" + port + ")/" + name +
		"?charset=utf8mb4&collation=utf8mb4_unicode_ci&parseTime=true&multiStatements=true"
}

func getenv(key, def string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return def
}

func bvnconnect(retries int) (*sql.DB, error) {
	var db *sql.DB
	var err error
	for i := 0; i < retries; i++ {
		db, err = sql.Open("mysql", dsn())
		if err == nil {
			err = db.Ping()
		}
		if err == nil {
			return db, nil
		}
		log.Printf("waiting for database (%d/%d): %v", i+1, retries, err)
		time.Sleep(2 * time.Second)
	}
	return nil, err
}

func initSchema(db *sql.DB) error {
	_, err := db.Exec(`CREATE TABLE IF NOT EXISTS notes (
		id         VARCHAR(64)  NOT NULL PRIMARY KEY,
		content    TEXT         NOT NULL,
		updated_at TIMESTAMP    NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP
	) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci`)
	return err
}

func loadNote(db *sql.DB, id string) (string, error) {
	var content string
	err := db.QueryRow(`SELECT content FROM notes WHERE id = ?`, id).Scan(&content)
	if err == sql.ErrNoRows {
		return "", nil
	}
	return content, err
}

func saveNote(db *sql.DB, id, content string) error {
	_, err := db.Exec(`INSERT INTO notes (id, content) VALUES (?, ?)
		ON DUPLICATE KEY UPDATE content = VALUES(content)`, id, content)
	return err
}

// ---- middleware & handlers ----

func cors() gin.HandlerFunc {
	return func(c *gin.Context) {
		c.Header("Access-Control-Allow-Origin", "*")
		c.Header("Access-Control-Allow-Methods", "GET, PUT, POST, OPTIONS")
		c.Header("Access-Control-Allow-Headers", "Content-Type")
		if c.Request.Method == http.MethodOptions {
			c.Status(http.StatusNoContent)
			c.Abort()
			return
		}
		c.Next()
	}
}

func validID(c *gin.Context) {
	if !idRe.MatchString(c.Param("id")) {
		c.Status(http.StatusNotFound)
		c.Abort()
	}
}
func main() {
	num1 := []int{1, 2, 3, 4, 5}
	for i := range num1 {
		if i == 3 {
			num1[i] |= i
		}
	}
	fmt.Println(num1)

}
