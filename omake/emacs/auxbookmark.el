;;; auxbookmark.el --- Consult Web Bookmarks powered by auxbookmark.exe -*- lexical-binding: t; -*-

;; Author: auxbookmark contributors
;; Keywords: bookmarks, consult, web, browser
;; Version: 0.1.0

;;; Commentary:
;;
;; auxbookmark.el provides a fast, interactive Web Bookmark search interface
;; for Emacs using `consult` and `auxbookmark.exe` as the multi-browser backend.
;;
;; Features:
;; - Cross-browser search: Search bookmarks across all browsers configured in auxbookmark.
;; - Consult incremental filtering: Search by title, browser name, or folder path.
;; - In-memory caching: Instantaneous opening (0ms) on repeated calls.
;;   Use prefix argument (`C-u M-x consult-auxbookmark`) to force refresh from disk.
;; - Browser launch: Press Enter to open the selected bookmark in your default browser.
;;
;; Setup Example (init.el):
;;
;;   (add-to-list 'load-path "C:/dev/auxbookmark/emacs")
;;   (require 'auxbookmark)
;;   (global-set-key (kbd "C-c b") 'consult-auxbookmark)
;;
;;   ;; With use-package:
;;   (use-package auxbookmark
;;     :load-path "C:/dev/auxbookmark/emacs"
;;     :bind ("C-c b" . consult-auxbookmark))

;;; Code:

(require 'cl-lib)
(require 'subr-x)

(defgroup auxbookmark nil
  "Multi-browser Web Bookmark search with Consult."
  :group 'convenience
  :prefix "auxbookmark-")

(defcustom auxbookmark-program
  (or (let ((p "C:/Program Files/PPX/auxcmd/auxbookmark.exe"))
        (when (file-exists-p p) p))
      (let ((p "C:/dev/auxbookmark/auxbookmark.exe"))
        (when (file-exists-p p) p))
      (executable-find "auxbookmark.exe")
      (executable-find "auxbookmark")
      "auxbookmark.exe")
  "Path to the auxbookmark.exe executable."
  :type 'file
  :group 'auxbookmark)

(defcustom auxbookmark-default-browser-action #'browse-url
  "Function to call with the bookmark URL when selected."
  :type 'function
  :group 'auxbookmark)

(defvar auxbookmark--cache nil
  "In-memory cached list of bookmark candidates.")

(defvar auxbookmark--last-fetch-time nil
  "Timestamp when `auxbookmark--cache` was last populated.")

(cl-defstruct (auxbookmark-entry (:constructor auxbookmark-entry-create))
  title
  url
  browser
  path)

(defun auxbookmark-clear-cache ()
  "Clear the in-memory bookmark cache."
  (interactive)
  (setq auxbookmark--cache nil
        auxbookmark--last-fetch-time nil)
  (message "[auxbookmark] Cache cleared."))

(defun auxbookmark--fetch-entries (&optional profile)
  "Fetch bookmark entries from `auxbookmark-program` TSV output.
If PROFILE is provided, fetch only for that browser."
  (unless (or (executable-find auxbookmark-program)
              (file-executable-p auxbookmark-program)
              (file-exists-p auxbookmark-program))
    (user-error "auxbookmark executable not found: %s" auxbookmark-program))
  (let* ((args (if profile (list "dump" profile) (list "dump")))
         (coding-system-for-read 'utf-8-dos)
         (output (with-output-to-string
                   (with-current-buffer standard-output
                     (apply #'process-file auxbookmark-program nil t nil args))))
         (lines (split-string output "[\r\n]+" t))
         (entries nil))
    (dolist (line lines)
      (let ((cols (split-string line "\t")))
        (when (>= (length cols) 4)
          (let ((title (nth 0 cols))
                (url   (nth 1 cols))
                (prof  (nth 2 cols))
                (path  (nth 3 cols)))
            (push (auxbookmark-entry-create
                   :title title
                   :url url
                   :browser prof
                   :path path)
                  entries)))))
    (nreverse entries)))

(defun auxbookmark--format-candidate (entry id)
  "Format an `auxbookmark-entry` into a string candidate with text properties.
ID is a unique sequence number to ensure candidate string uniqueness."
  (let* ((browser (auxbookmark-entry-browser entry))
         (title   (auxbookmark-entry-title entry))
         (path    (auxbookmark-entry-path entry))
         (url     (auxbookmark-entry-url entry))
         (suffix  (propertize (format "\0%d" id) 'invisible t))
         (display (format "%-50s [%s / %s]%s"
                          title
                          (propertize browser 'face 'font-lock-keyword-face)
                          (propertize path 'face 'font-lock-comment-face)
                          suffix)))
    (propertize display
                'auxbookmark-entry entry
                'auxbookmark-url url
                'auxbookmark-browser browser)))

(defun auxbookmark--get-candidates (refresh)
  "Return cached candidates, or fetch anew if REFRESH or cache is nil."
  (if (or refresh (null auxbookmark--cache))
      (progn
        (message "[auxbookmark] Fetching bookmarks from %s..." (file-name-nondirectory auxbookmark-program))
        (let* ((entries (auxbookmark--fetch-entries))
               (id 0)
               (candidates (mapcar (lambda (entry)
                                     (setq id (1+ id))
                                     (auxbookmark--format-candidate entry id))
                                   entries)))
          (setq auxbookmark--cache candidates
                auxbookmark--last-fetch-time (current-time))
          (message "[auxbookmark] Loaded %d bookmarks." (length candidates))
          candidates))
    auxbookmark--cache))

(defun auxbookmark--lookup-candidate (selected candidates)
  "Retrieve the original candidate with text properties for SELECTED from CANDIDATES."
  (or (and (get-text-property 0 'auxbookmark-url selected) selected)
      (car (member selected candidates))))

;;;###autoload
(defun consult-auxbookmark (&optional refresh)
  "Search all Web Bookmarks with Consult and open the selected in browser.
With prefix argument REFRESH (C-u), reload bookmarks from disk."
  (interactive "P")
  (let ((candidates (auxbookmark--get-candidates refresh)))
    (if (null candidates)
        (message "No bookmarks found.")
      (let* ((selected
              (if (fboundp 'consult--read)
                  ;; Consult UI
                  (consult--read
                   candidates
                   :prompt "Web Bookmark: "
                   :category 'auxbookmark
                   :sort nil
                   :require-match t
                   :lookup (or (and (fboundp 'consult--lookup-member) #'consult--lookup-member)
                               (lambda (sel cands &rest _) (car (member sel cands))))
                   :annotate
                   (lambda (cand)
                     (let ((url (get-text-property 0 'auxbookmark-url cand)))
                       (when url
                         (format "  %s" (propertize url 'face 'font-lock-comment-face))))))
                ;; Fallback to completing-read if consult is not loaded
                (completing-read "Web Bookmark: " candidates nil t))))
        (when selected
          (let* ((cand (auxbookmark--lookup-candidate selected candidates))
                 (url  (and cand (get-text-property 0 'auxbookmark-url cand))))
            (if (and url (not (string-empty-p url)))
                (progn
                  (message "Opening: %s" url)
                  (funcall auxbookmark-default-browser-action url))
              (message "No URL found for selected bookmark."))))))))

;;;###autoload
(defalias 'auxbookmark-consult #'consult-auxbookmark)

(provide 'auxbookmark)

;;; auxbookmark.el ends here
